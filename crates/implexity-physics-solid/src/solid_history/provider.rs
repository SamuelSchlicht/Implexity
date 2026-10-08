// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use implexity_core::CaeError;
use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, LegacySingleArrayProviderCapabilities, ProviderCapabilities,
    ProviderProblem, Sensitivity,
};
use implexity_core::coupling_graph::CouplingDeclaration;
use implexity_core::orchestration::{
    AddInCategory, AddInContract, DesignCoordinateRef, ExecutionKind, Fidelity, PublishedContract,
    ResponseCapability, RuntimeRoute,
};
use implexity_core::packages::InstallContext;
use implexity_optim::design::{NamedArrays, design_identity};
use implexity_optim::provider_ops::{DesignOp, DesignOperations, DesignSensitivities, DesignSensitivity};

use super::kernel::SolidKernel;
use super::{COORDS, LIMITATIONS, RESPONSES, UNITS, normalise, py_shape};
use crate::components::{SolidComponent, register_strict};
use crate::util::contract;

pub const NAME: &str = "native_solid_history";

#[derive(Default)]
pub struct NativeSolidHistoryProvider {
    cache: Mutex<Vec<(String, Arc<SolidKernel>)>>,
}

impl std::fmt::Debug for NativeSolidHistoryProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeSolidHistoryProvider")
    }
}

fn arr(values: Vec<f64>, shape: &[usize]) -> Result<ArrayD<f64>, CaeError> {
    ArrayD::from_shape_vec(IxDyn(shape), values)
        .map_err(|e| CaeError::contract(format!("internal array shape error: {e}")))
}

fn problem_value(problem: &ProviderProblem) -> Result<&Value, CaeError> {
    problem
        .downcast_ref::<Value>()
        .ok_or_else(|| CaeError::contract("native_solid_history requires its own normalised problem mapping"))
}

impl NativeSolidHistoryProvider {
    pub const IMPLEMENTATION: &'static str =
        "implexity.physics_library.solid_history.NativeSolidHistoryProvider";

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn kernel(&self, p: &Value) -> Result<Arc<SolidKernel>, CaeError> {
        let token = implexity_core::registries::global().addins.binding_token();
        let key = format!(
            "{}|{}|{}",
            token.generation,
            token.fingerprint,
            serde_json::to_string(p).unwrap_or_default()
        );
        if let Ok(cache) = self.cache.lock()
            && let Some((_, k)) = cache.iter().find(|(k, _)| *k == key)
        {
            return Ok(Arc::clone(k));
        }
        let kernel = Arc::new(SolidKernel::new(p.clone())?);
        if let Ok(mut cache) = self.cache.lock() {
            if cache.len() >= 4 {
                cache.remove(0);
            }
            cache.push((key, Arc::clone(&kernel)));
        }
        Ok(kernel)
    }


    pub fn parts(
        &self,
        problem: &Value,
        design: &NamedArrays,
    ) -> Result<(Value, Arc<SolidKernel>, Vec<f64>), CaeError> {
        let p = normalise(problem)?;
        let names = design.names();
        if names.len() != 3 || COORDS.iter().any(|c| !design.contains(c)) {
            return contract(format!(
                "exact native coordinates required: ({})",
                COORDS.iter().map(|c| implexity_core::py_repr::repr_str(c)).collect::<Vec<_>>().join(", ")
            ));
        }
        let grid: Vec<usize> = p["grid"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|g| usize::try_from(g.as_u64().unwrap_or(0)).unwrap_or(0))
            .collect();
        let get = |name: &str, shape: &[usize], label: &str| -> Result<Vec<f64>, CaeError> {
            let a = design.get(name).ok_or_else(|| CaeError::contract("missing design coordinate"))?;
            if a.shape() != shape || a.iter().any(|v| !v.is_finite()) {
                return contract(format!(
                    "{label}: expected finite array {}, received {}",
                    py_shape(shape),
                    py_shape(a.shape())
                ));
            }
            Ok(a.iter().copied().collect())
        };
        let rho = get(COORDS[0], &grid, "topology")?;
        let h = get(COORDS[1], &[3], "native sample spacing [mm]")?;
        let c = get(COORDS[2], &grid, "material fraction")?;
        if rho.iter().any(|v| *v <= 0.0 || *v > 1.0)
            || c.iter().any(|v| *v < 0.0 || *v > 1.0)
            || h.iter().any(|v| *v <= 0.0)
        {
            return contract("topology must be in (0,1], material in [0,1], spacing positive");
        }
        let mut x = rho;
        x.extend(h);
        x.extend(c);
        let k = self.kernel(&p)?;
        Ok((p, k, x))
    }

    fn gradients(vector: &[f64], k: &SolidKernel) -> Result<NamedArrays, CaeError> {
        let n = k.nc;
        let g = k.grid.to_vec();
        Ok(NamedArrays::from_pairs([
            (COORDS[0].to_string(), arr(vector[..n].to_vec(), &g)?),
            (COORDS[1].to_string(), arr(vector[n..n + 3].to_vec(), &[3])?),
            (COORDS[2].to_string(), arr(vector[n + 3..].to_vec(), &g)?),
        ]))
    }

    fn registration(k: &SolidKernel, spacing: &[f64]) -> Result<Value, CaeError> {
        let bounds: [f64; 3] = std::array::from_fn(|a| k.grid[a] as f64 * spacing[a]);
        implexity_geometry::field_registration::axis_aligned_registration(k.grid, [0.0; 3], bounds, "cell")
            .map(|r| r.to_wire())
            .map_err(|e| CaeError::contract(e.to_string()))
    }

    #[allow(clippy::too_many_lines)]
    fn diagnostics(
        p: &Value,
        k: &SolidKernel,
        x: &[f64],
        states: &[Vec<f64>],
        sol: &implexity_solve::native_history::HistorySolution,
        design: &NamedArrays,
    ) -> Result<Map<String, Value>, CaeError> {
        let last = states.len() - 1;
        let d = k.observe(last, &states[last], &states[last - 1], x);
        let validity = k.material_validity(last, &states[last], x);
        let spacing = [x[k.nc], x[k.nc + 1], x[k.nc + 2]];
        let registration = Self::registration(k, &spacing)?;
        let m = &k.model;
        let policy = p.get("inactive_phase_numerical_material").filter(|v| !v.is_null());
        let pv = |key: &str| policy.map_or(Value::Null, |p| p[key].clone());
        let max = |v: &[f64]| v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let plastic_metadata =
            m.plastic.map(|pl| pl.state_metadata(&m.materials)).transpose()?.unwrap_or_default();
        let mut internal_fields = vec![json!("plastic_strain_mandel_6"), json!("equivalent_plastic_strain")];
        internal_fields.extend(plastic_metadata.iter().map(|r| r["name"].clone()));
        internal_fields.extend([json!("creep_strain_mandel_6"), json!("equivalent_creep_strain")]);
        let t_last = k.nodal_temperature(last, &states[last]);
        let mut out = crate::util::obj(json!({
            "field_registration": registration,
            "design_field_registrations": {COORDS[0]: registration, COORDS[2]: registration},
            "fatigue_observer": k.fatigue_diagnostics(states, x)?,
            "components": p["components"], "time_steps": k.nt - 1, "state_unknowns_per_step": k.state_size,
            "assembly": k.local.report(),
            "state_residual_norms": sol.residual_norms, "newton_iterations": sol.newton_iterations,
            "max_state_residual": max(&sol.residual_norms),
            "design_state_id": design_identity(design)?,
            "maximum_von_mises_Pa": max(&d.von_mises),
            "maximum_plastic_strain": max(&d.equivalent_plastic),
            "maximum_creep_strain": max(&d.equivalent_creep),
            "material_provenance": m.materials.iter().map(crate::material::SolidMaterial::provenance).collect::<Vec<_>>(),
            "solid_material_validity": validity,
            "inactive_phase_numerical_material": {
                "enabled": m.numerical, "schema": pv("schema"), "method": pv("method"), "scope": pv("scope"), "provenance": pv("provenance"),
                "endpoint_match_continuity": "C1_value_and_first_derivative; base PCHIP is not generally C2 at its authored endpoints",
                "physical_material_extrapolation_authorized": p["applicability_policy"].as_str() == Some("report_only"),
                "numerical_continuation_scope": if p["applicability_policy"].as_str() == Some("report_only") { "all_positive_temperature_states_for_optimization_exploration" } else { "inactive_phase_only" },
                "physical_validity_gate": p["applicability_policy"].as_str() != Some("report_only"),
                "material_history_addin_supported": m.history.as_ref().is_some_and(|h| h.numerical_extension.is_some()),
                "material_history_numerical_extension": m.history.as_ref().map_or(Value::Null, crate::history::MaterialHistoryBinding::numerical_extension_report),
                "physical_qualification": false},
            "applicability_policy": p.get("applicability_policy").cloned().unwrap_or(json!("enforce")),
            "regime_valid": true,
            "regime_valid_semantics": "retained_numerical_checks_and_Newton_convergence; physical_applicability_is_reported_separately",
            "applicability_screens_passed": states.iter().enumerate().all(|(n, z)| k.material_validity(n, z, x)["solid_temperature_material_interval_screen_passed"] == json!(true)),
            "constitutive_model": if m.viscoelastic.is_some() { "native_maxwell_polymer" } else { "small_strain_mixed_J2_and_Norton_as_selected" },
            "limitations": LIMITATIONS,
            "response_units": RESPONSES.iter().zip(UNITS).map(|(r, u)| ((*r).to_string(), json!(u))).collect::<Map<_, _>>(),
            "jacobian_assembly": "element_local_automatic_differentiation_sparse_incidence",
            "yield_switch_certificate": k.yield_switch_report(states, x),
            "thermal_exchange": {"reservoirs": p["thermal_reservoirs"], "exchanges": p["thermal_exchanges"],
                "powers_final": k.boundary.powers(last, &states[last], x, &t_last)},
            "state_layout": {"temperature_free_nodes": k.free_t, "displacement_free_dofs": k.free_u,
                "internal_mandel_order": ["xx", "yy", "zz", "sqrt2_yz", "sqrt2_xz", "sqrt2_xy"],
                "material_history_states": m.history.as_ref().map_or_else(Vec::new, |h| h.metadata.clone()),
                "internal_fields": internal_fields,
                "plastic_size": m.layout.plastic_size, "creep_offset": m.layout.plastic_size,
                "creep_size": m.layout.creep_size,
                "viscoelastic_offset": m.layout.plastic_size + m.layout.creep_size,
                "viscoelastic_size": m.layout.viscoelastic_size,
                "viscoelastic_settings": m.viscoelastic.as_ref().map_or(Value::Null, |v| v.settings.clone()),
                "internal_scales": m.scales,
                "material_history_offset": m.layout.material_start(),
                "disabled_constitutive_diagnostics": {
                    "plasticity": if m.plastic.is_some() { "stored_native_state" } else { "exact_zero_reconstruction_no_history_coordinates" },
                    "creep": if m.creep.is_some() { "stored_native_state" } else { "exact_zero_reconstruction_no_history_coordinates" }},
                "identity_coordinates_eliminated_per_tetrahedron": (if m.plastic.is_some() { 0 } else { 7 }) + (if m.creep.is_some() { 0 } else { 7 }),
                "plastic_additional_metadata": plastic_metadata,
                "finite_reservoir_indices": k.boundary.indices, "temperature_reference_K": m.t0, "temperature_scale_K": m.ts,
                "displacement_scale_m": m.us, "strain_scale": m.es,
                "velocity_free_dofs_state_slice": [k.velocity.start, k.velocity.end],
                "acceleration_free_dofs_state_slice": [k.acceleration.start, k.acceleration.end]},
            "physical_qualification": false}));
        out.insert(
            "structural_dynamics".into(),
            match (&k.dynamics, &k.inertia) {
                (Some(_), Some(inertia)) => json!({"settings": p["structural_dynamics"], "inertia_assembly": inertia.report(),
                    "energy_ledger": k.energy_ledger(states, x), "limitations": crate::structural_inertia::LIMITATIONS}),
                _ => Value::Null,
            },
        );
        Ok(out)
    }

    fn cell(k: &SolidKernel, values: &[f64], extra: &[usize]) -> Result<ArrayD<f64>, CaeError> {
        let width: usize = extra.iter().product::<usize>().max(1);
        let mut out = vec![0.0; k.nc * width];
        for cell in 0..k.nc {
            for j in 0..width {
                let mut acc = 0.0;
                for t in 0..6 {
                    acc += values[(cell * 6 + t) * width + j];
                }
                out[cell * width + j] = acc / 6.0;
            }
        }
        let mut shape = k.grid.to_vec();
        shape.extend_from_slice(extra);
        arr(out, &shape)
    }


    #[allow(clippy::too_many_lines)]
    pub fn evaluate_named(&self, problem: &Value, design: &NamedArrays) -> Result<Evaluation, CaeError> {
        let (p, k, x) = self.parts(problem, design)?;
        let sol = k.solve(&x)?;
        let states = &sol.states;
        let values = k.responses(states, &x, false)?.values;
        let responses: BTreeMap<String, f64> =
            RESPONSES.iter().zip(values).map(|(r, v)| ((*r).to_string(), v)).collect();
        let mut dg = Self::diagnostics(&p, &k, &x, states, &sol, design)?;
        let last = states.len() - 1;
        let d = k.observe(last, &states[last], &states[last - 1], &x);
        let m = &k.model;
        let grid = k.grid.to_vec();
        let flat6 = |v: &[[f64; 6]]| v.iter().flatten().copied().collect::<Vec<_>>();
        let element_t: Vec<f64> = k
            .mesh
            .tets
            .iter()
            .map(|t| t.iter().map(|n| d.temperature_nodes[*n]).sum::<f64>() / 4.0)
            .collect();
        let mut fields: BTreeMap<String, FieldValue> = BTreeMap::new();
        let mut put = |name: &str, a: ArrayD<f64>| {
            fields.insert(name.to_string(), FieldValue::Array(a));
        };
        put("solid_fraction", design.get(COORDS[0]).cloned().unwrap_or_default());
        put("material_fraction", design.get(COORDS[2]).cloned().unwrap_or_default());
        put("temperature_K", Self::cell(&k, &element_t, &[])?);
        put("von_mises_Pa", Self::cell(&k, &d.von_mises, &[])?);
        put("equivalent_plastic_strain", Self::cell(&k, &d.equivalent_plastic, &[])?);
        put("equivalent_creep_strain", Self::cell(&k, &d.equivalent_creep, &[])?);
        put("stress_mandel_Pa", Self::cell(&k, &flat6(&d.stress), &[6])?);
        let nt = states.len();
        let th: Vec<f64> = (0..nt).flat_map(|n| k.nodal_temperature(n, &states[n])).collect();
        put("temperature_nodes_history_K", arr(th, &[nt, k.nn])?);
        let uh: Vec<f64> =
            (0..nt).flat_map(|n| k.nodal_displacement(n, &states[n]).into_iter().flatten()).collect();
        put("displacement_nodes_history_m", arr(uh, &[nt, k.nn, 3])?);
        put(
            "state_history_nondimensional",
            arr(states.iter().flatten().copied().collect(), &[nt, k.state_size])?,
        );
        put("tetrahedral_stress_mandel_Pa", arr(flat6(&d.stress), &[k.ne, 6])?);
        let spacing_m: [f64; 3] = std::array::from_fn(|a| x[k.nc + a] * 1e-3);
        let mesh_nodes: Vec<f64> =
            k.mesh.ijk.iter().flat_map(|v| (0..3).map(move |a| v[a] as f64 * spacing_m[a])).collect();
        put("mesh_nodes_m", arr(mesh_nodes, &[k.nn, 3])?);
        put("tetrahedron_nodes", arr(k.mesh.tets.iter().flatten().map(|v| *v as f64).collect(), &[k.ne, 4])?);
        put("tetrahedron_native_cell", arr(k.mesh.owners.iter().map(|v| *v as f64).collect(), &[k.ne])?);
        put("times_s", arr(k.times.clone(), &[nt])?);
        if let Some(loads) = p.get("nodal_forces_N").filter(|v| !v.is_null()) {
            let (shape, v) = crate::util::real_array(loads).unwrap_or_default();
            put("prescribed_nodal_forces_history_N", arr(v, &shape)?);
        }
        if k.dynamics.is_some() {
            let (mut vh, mut ah) = (Vec::new(), Vec::new());
            for z in states {
                let (v, a) = k.kinematic_nodes(z);
                vh.extend(v.into_iter().flatten());
                ah.extend(a.into_iter().flatten());
            }
            put("velocity_nodes_history_m_s", arr(vh, &[nt, k.nn, 3])?);
            put("acceleration_nodes_history_m_s2", arr(ah, &[nt, k.nn, 3])?);
        }
        let base = k.n_t() + k.n_u();
        let internal_rows = |range: std::ops::Range<usize>| -> Vec<f64> {
            let mut out = Vec::new();
            for z in states {
                for e in 0..k.ne {
                    let start = base + e * k.internal_size;
                    out.extend(
                        z[start..start + k.internal_size][range.clone()]
                            .iter()
                            .zip(&m.scales[range.clone()])
                            .map(|(v, s)| v * s),
                    );
                }
            }
            out
        };
        if let Some(h) = &m.history {
            let range = m.layout.material_start()..k.internal_size;
            put("material_state_history", arr(internal_rows(range.clone()), &[nt, k.ne, range.len()])?);
            put("material_stored_energy_J_m3", Self::cell(&k, &d.material_stored_energy, &[])?);
            put("material_conductivity_W_mK", Self::cell(&k, &d.conductivity, &[])?);
            put("material_yield_stress_Pa", Self::cell(&k, &d.yield_stress, &[])?);
            if h.endpoints.is_some() {
                let mut support = Vec::new();
                for e in 0..k.ne {
                    let o = k.mesh.owners[e];
                    support.extend(h.physical_support(x[o], x[k.nc + 3 + o])?);
                }
                put("material_state_support_fraction", arr(support, &[k.ne, h.size])?);
            }
        }
        if let Some(v) = &m.viscoelastic {
            let range = m.layout.viscoelastic();
            put("viscoelastic_state_history", arr(internal_rows(range.clone()), &[nt, k.ne, range.len()])?);
            for name in [
                "viscoelastic_stored_energy_J_m3",
                "viscoelastic_heat_increment_J_m3",
                "viscoelastic_assembled_heat_increment_J_m3",
                "viscoelastic_assembled_numerical_dissipation_increment_J_m3",
            ] {
                let col = d.polymer_column(name.trim_start_matches("viscoelastic_"));
                put(name, Self::cell(&k, &col, &[])?);
            }
            let _ = v;
        }
        if let Some(defect) = &d.thermoelastic_defect {
            put("thermoelastic_numerical_energy_defect_J_m3", Self::cell(&k, defect, &[])?);
        }
        if !k.boundary.reservoirs.is_empty() {
            let names: Vec<String> = k.boundary.reservoirs.iter().map(|r| r.id.clone()).collect();
            let hist: Vec<f64> = (0..nt)
                .flat_map(|n| k.boundary.temperatures(n, &states[n]).into_iter().map(|(_, t)| t))
                .collect();
            fields.insert(
                "reservoir_temperature_history_K".into(),
                FieldValue::Array(arr(hist, &[nt, names.len()])?),
            );
            dg["thermal_exchange"]["reservoir_field_order"] = json!(names);
            let powers: Vec<Value> = (1..nt)
                .map(|n| k.boundary.powers(n, &states[n], &x, &k.nodal_temperature(n, &states[n])))
                .collect();
            dg["thermal_exchange"]["powers_history"] = json!(powers);
        }
        let reg = Self::registration(&k, &[x[k.nc], x[k.nc + 1], x[k.nc + 2]])?;
        let metadata = field_metadata(&k, &fields, &reg, &grid);
        dg.insert("field_metadata".into(), metadata);
        dg.insert("field_registration".into(), reg);
        Ok(Evaluation { provider: NAME.into(), responses, diagnostics: dg, fields })
    }


    pub fn sensitivities_named(
        &self,
        problem: &Value,
        design: &NamedArrays,
        responses: &[String],
    ) -> Result<DesignSensitivities, CaeError> {
        let (p, k, x) = self.parts(problem, design)?;
        let sol = k.solve(&x)?;
        let mut unique = responses.to_vec();
        unique.sort();
        unique.dedup();
        if unique.len() != responses.len()
            || responses.is_empty()
            || responses.iter().any(|r| !RESPONSES.contains(&r.as_str()))
        {
            return contract("unknown, empty or duplicated solid response request");
        }
        let indices: Vec<usize> =
            responses.iter().map(|r| RESPONSES.iter().position(|q| q == r).unwrap_or(0)).collect();
        k.certify_sensitivity(&sol.states, &x)?;
        let eval = k.responses(&sol.states, &x, true)?;
        let system = k.system()?;
        let out =
            system.adjoint_many(&x, &sol, &eval.gu_dense(&indices), &eval.gx_dense(&indices), None, None)?;
        let mut dg = Self::diagnostics(&p, &k, &x, &sol.states, &sol, design)?;
        dg.insert("adjoint_factorizations".into(), json!(out.adjoint_factorizations));
        dg.insert("adjoint_factorization_builds".into(), json!(out.adjoint_factorization_builds));
        dg.insert("adjoint_factorization_reuses".into(), json!(out.adjoint_factorization_reuses));
        dg.insert(
            "maximum_transpose_relative_residual".into(),
            json!(out.maximum_transpose_relative_residual),
        );
        dg.insert("history_states_retained".into(), json!(out.history_states_retained));
        dg.insert("history_derivative".into(), json!(out.history_derivative));
        let mut result = DesignSensitivities { diagnostics: dg, ..DesignSensitivities::default() };
        for (j, (r, i)) in responses.iter().zip(&indices).enumerate() {
            result.responses.insert(r.clone(), eval.values[*i]);
            let column: Vec<f64> =
                (0..x.len()).map(|row| out.gradients.data[row * indices.len() + j]).collect();
            result.gradients.insert(r.clone(), Self::gradients(&column, &k)?);
        }
        Ok(result)
    }


    pub fn declaration(problem: &Value) -> Result<CouplingDeclaration, CaeError> {
        let components = problem.get("components").cloned().unwrap_or_else(|| json!({}));
        let reversible = components.get("material") == Some(&json!("constant_strain_thermoelastic_solid"));
        let dissipative =
            ["plasticity", "creep"].iter().any(|k| components.get(*k).is_some_and(|v| !v.is_null()))
                || problem.get("viscoelasticity").is_some_and(|v| !v.is_null());
        let mut edges = vec![crate::coupling::edge(
            "thermal",
            "structure",
            "temperature_field",
            "monolithic",
            "thermal strain and E/yield(T)",
        )];
        if reversible || dissipative {
            edges.push(crate::coupling::edge(
                "structure",
                "thermal",
                if reversible { "reversible_thermoelastic_heat" } else { "inelastic_dissipation_heat" },
                "monolithic",
                if reversible {
                    "explicit Helmholtz entropy coupling"
                } else {
                    "selected plastic/creep/viscoelastic heat in energy equation"
                },
            ));
        }
        let closed = edges.len() > 1;
        let base = CouplingDeclaration {
            provider: NAME.into(),
            active_physics: vec!["thermal".into(), "structure".into()],
            ports: Vec::new(),
            edges,
            closed_loops: if closed { vec![vec!["thermal".into(), "structure".into()]] } else { Vec::new() },
            intentionally_frozen: Vec::new(),
            notes: vec!["Small-strain reference geometry; no geometric FSI or contact is claimed.".into()],
        };
        let with_material = crate::coupling::with_material_couplings(
            base,
            problem.get("material_history").unwrap_or(&Value::Null),
        )?;
        Ok(crate::polymer::with_environmental_maxwell_couplings(with_material, problem))
    }


    pub fn contract() -> Result<AddInContract, CaeError> {
        let mut c = AddInContract::new(NAME);
        c.category = AddInCategory::Field;
        let inputs: Vec<DesignCoordinateRef> = COORDS
            .iter()
            .enumerate()
            .map(|(i, coord)| {
                let mut d = DesignCoordinateRef::new(*coord, format!("{NAME}.design.{i}"));
                d.addin_id = NAME.into();
                d
            })
            .collect();
        let deps: Vec<String> = inputs.iter().map(|d| d.port_id.clone()).collect();
        c.responses = RESPONSES
            .iter()
            .zip(UNITS)
            .map(|(r, u)| {
                let mut cap = ResponseCapability::new(*r);
                cap.unit = u.into();
                cap.differentiable = Some(true);
                cap.design_reachable = Some(true);
                cap.depends_on.clone_from(&deps);
                cap
            })
            .collect();
        c.scope = vec!["*".into()];
        c.fidelity = Fidelity::Intermediate;
        c.priority = 50;
        c.runtime_route = RuntimeRoute::Array;
        c.exact_design_derivatives = Some(true);
        c.exact_state_transpose = Some(true);
        c.notes = LIMITATIONS.iter().map(|s| (*s).to_string()).collect();
        c.contract_version = 2;
        c.compatibility_mode = false;
        c.owner_id = format!("provider:{NAME}");
        c.execution_kind = Some(ExecutionKind::Provider);
        c.supported_operations = [
            "preflight",
            "preflight_design",
            "evaluate",
            "sensitivity",
            "sensitivities",
            "optimize",
            "accept_design",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        c.no_op_operations = Vec::new();
        c.design_inputs = inputs;
        c.checked()
    }
}

fn field_metadata(
    k: &SolidKernel,
    fields: &BTreeMap<String, FieldValue>,
    reg: &Value,
    grid: &[usize],
) -> Value {
    let m = &k.model;
    let cell_names = [
        "solid_fraction",
        "material_fraction",
        "temperature_K",
        "von_mises_Pa",
        "equivalent_plastic_strain",
        "equivalent_creep_strain",
        "stress_mandel_Pa",
    ];
    let last_time = k.times[k.nt - 1];
    let mut out = Map::new();
    for (name, value) in fields {
        let shape: Vec<usize> = match value {
            FieldValue::Array(a) => a.shape().to_vec(),
            FieldValue::Json(_) => Vec::new(),
        };
        let mut units = if name.ends_with("_Pa") {
            "Pa"
        } else if name.ends_with("_K") {
            "K"
        } else if name == "mesh_nodes_m" {
            "m"
        } else if name == "times_s" {
            "s"
        } else {
            "1"
        }
        .to_string();
        let mut iscell = shape.len() >= 3 && shape[..3] == grid[..] && cell_names.contains(&name.as_str());
        if name == "material_state_history" {
            units = "1".into();
        }
        if name == "material_stored_energy_J_m3" {
            units = "J/m^3".into();
        }
        if name.starts_with("viscoelastic_") && name.ends_with("_J_m3") {
            units = "J/m^3".into();
            iscell = true;
        }
        if name == "thermoelastic_numerical_energy_defect_J_m3" {
            units = "J/m^3".into();
            iscell = true;
        }
        if name == "material_conductivity_W_mK" {
            units = "W/(m K)".into();
        }
        if name.starts_with("material_")
            && name != "material_state_history"
            && name != "material_state_support_fraction"
        {
            iscell = true;
        }
        let mut row = json!({"units": units, "association": if iscell { "cell" } else { "history_or_tetrahedron" },
            "source": if iscell { "native_field_solver_cell_average" } else { "native_field_solver_exact" },
            "rank": if name == "stress_mandel_Pa" { "vector" } else { "scalar" }});
        let set = |row: &mut Value, extra: Value| {
            for (k2, v) in extra.as_object().cloned().unwrap_or_default() {
                row[k2] = v;
            }
        };
        match name.as_str() {
            "material_state_history" => {
                if let Some(h) = &m.history {
                    let units: Vec<&Value> = h.metadata.iter().map(|r| &r["units"]).collect();
                    let same = units.iter().all(|u| *u == units[0]);
                    set(
                        &mut row,
                        json!({"association": "material_point_history", "axes": ["time", "tetrahedron", "state"],
                        "state_metadata": h.metadata, "units": if same { units[0].clone() } else { json!("mixed_explicit_states") }}),
                    );
                    if h.endpoints.is_some() {
                        set(
                            &mut row,
                            json!({"physical_support_field": "material_state_support_fraction",
                            "state_interpretation": "potential_inventory; physical_only_where_support_is_positive",
                            "numerical_extension": h.numerical_extension_report()}),
                        );
                    }
                }
            }
            "material_state_support_fraction" => {
                if let Some(h) = &m.history {
                    set(
                        &mut row,
                        json!({"association": "material_point_state_support", "axes": ["tetrahedron", "state"],
                        "state_endpoints": h.endpoints, "source": "exact_physical_endpoint_volume_fraction_no_threshold"}),
                    );
                }
            }
            "viscoelastic_state_history" => {
                if let Some(v) = &m.viscoelastic {
                    let meta = v.metadata();
                    let units: Vec<&Value> = meta.iter().map(|r| &r["units"]).collect();
                    let same = units.iter().all(|u| *u == units[0]);
                    set(
                        &mut row,
                        json!({"association": "material_point_history", "axes": ["time", "tetrahedron", "state"],
                        "state_metadata": meta, "units": if same { units[0].clone() } else { json!("mixed_explicit_states") }}),
                    );
                }
            }
            "mesh_nodes_m" => set(
                &mut row,
                json!({"association": "node", "rank": "vector", "components": ["x", "y", "z"], "configuration": "reference", "geometric_role": "position", "coordinate_units": "m"}),
            ),
            "tetrahedron_nodes" => set(
                &mut row,
                json!({"association": "tetrahedron_connectivity", "index_base": 0, "node_field": "mesh_nodes_m"}),
            ),
            "tetrahedron_native_cell" => set(
                &mut row,
                json!({"association": "tetrahedron", "index_base": 0, "cell_order": "C", "cell_grid": grid, "registration": reg}),
            ),
            "temperature_nodes_history_K" => set(
                &mut row,
                json!({"association": "node_history", "axes": ["time", "node"], "reference_coordinate_field": "mesh_nodes_m"}),
            ),
            "displacement_nodes_history_m" => set(
                &mut row,
                json!({"units": "m", "association": "node_history", "axes": ["time", "node", "component"], "rank": "vector", "components": ["x", "y", "z"], "reference_coordinate_field": "mesh_nodes_m"}),
            ),
            "velocity_nodes_history_m_s" | "acceleration_nodes_history_m_s2" => set(
                &mut row,
                json!({"units": if name.ends_with("_m_s") { "m/s" } else { "m/s^2" }, "association": "node_history", "axes": ["time", "node", "component"], "rank": "vector", "components": ["x", "y", "z"], "reference_coordinate_field": "mesh_nodes_m", "source": "native_newmark_state"}),
            ),
            "prescribed_nodal_forces_history_N" => set(
                &mut row,
                json!({"units": "N", "association": "node_history", "axes": ["time", "node", "component"], "rank": "vector", "components": ["x", "y", "z"], "reference_coordinate_field": "mesh_nodes_m", "source": "authored_dead_load", "excludes": "face tractions and support reactions"}),
            ),
            "times_s" => set(
                &mut row,
                json!({"association": "time_coordinates", "coordinate_for": "stored_state_history"}),
            ),
            "reservoir_temperature_history_K" => set(
                &mut row,
                json!({"association": "thermal_reservoir_history", "axes": ["time", "reservoir"],
                "reservoir_ids": k.boundary.reservoirs.iter().map(|r| r.id.clone()).collect::<Vec<_>>()}),
            ),
            _ => {}
        }
        if iscell {
            row["registration"] = reg.clone();
            if name == "solid_fraction" || name == "material_fraction" {
                row["temporal_association"] = json!("time_invariant_design_derived");
            } else {
                set(
                    &mut row,
                    json!({"temporal_association": "final_stored_state", "time_index": k.nt - 1, "time_s": last_time}),
                );
            }
        }
        if name == "stress_mandel_Pa" || name == "tetrahedral_stress_mandel_Pa" {
            set(
                &mut row,
                json!({"rank": "tensor", "components": ["xx", "yy", "zz", "sqrt2_yz", "sqrt2_xz", "sqrt2_xy"], "tensor_convention": "orthonormal_Mandel"}),
            );
            if name == "tetrahedral_stress_mandel_Pa" {
                set(
                    &mut row,
                    json!({"association": "tetrahedron", "temporal_association": "final_stored_state", "time_index": k.nt - 1, "time_s": last_time}),
                );
            }
        }
        out.insert(name.clone(), row);
    }
    Value::Object(out)
}

fn missing(method: &str) -> CaeError {
    CaeError::contract(format!("'NativeSolidHistoryProvider' object has no attribute '{method}'"))
}

impl CaeProvider for NativeSolidHistoryProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn implementation(&self) -> &str {
        Self::IMPLEMENTATION
    }

    fn capabilities(&self) -> Result<ProviderCapabilities, CaeError> {
        let mut caps = LegacySingleArrayProviderCapabilities::new(
            NAME,
            ["thermal", "structural", "plasticity", "creep", "structural_dynamics"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            RESPONSES.iter().map(|s| (*s).to_string()).collect(),
        );
        caps.base.fields =
            ["temperature_K", "von_mises_Pa", "equivalent_plastic_strain", "equivalent_creep_strain"]
                .iter()
                .map(|s| (*s).to_string())
                .collect();
        caps.base.nonlinear = true;
        caps.base.design_coordinates = COORDS.iter().map(|s| (*s).to_string()).collect();
        caps.editor = crate::util::obj(json!({"kind": "native_json", "title": "Coupled solid history",
            "required_packages": ["solid_mechanics", "inelastic_materials"],
            "problem_template": super::solid_history_starter(),
            "schema": {"properties": super::solid_editor_properties(true)}}));
        let mut notes: Vec<String> = LIMITATIONS.iter().map(|s| (*s).to_string()).collect();
        notes.push("Constitutive components must be explicitly loaded and selected.".into());
        caps.base.notes = notes;
        Ok(ProviderCapabilities::Legacy(Box::new(caps)))
    }

    fn orchestration_contract(&self) -> Option<Result<PublishedContract, CaeError>> {
        Some(Self::contract().map(|c| PublishedContract::Contract(Box::new(c))))
    }

    fn normalise_problem(&self, problem: &Value) -> Result<ProviderProblem, CaeError> {
        Ok(Arc::new(normalise(problem)?))
    }

    fn preflight(
        &self,
        problem: &ProviderProblem,
        _topology: Option<&ArrayD<f64>>,
    ) -> Result<Map<String, Value>, CaeError> {
        let p = normalise(problem_value(problem)?)?;
        Ok(crate::util::obj(json!({"ok": true, "issues": [], "components": p["components"],
            "requires_complete_design": true, "physical_qualification": false})))
    }

    fn evaluate(&self, _problem: &ProviderProblem, _topology: &ArrayD<f64>) -> Result<Evaluation, CaeError> {
        Err(missing("evaluate"))
    }

    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        _topology: &ArrayD<f64>,
        _response: &str,
    ) -> Result<Sensitivity, CaeError> {
        Err(missing("sensitivity"))
    }

    fn coupling_declaration(&self, problem: Option<&ProviderProblem>) -> Option<Result<Value, CaeError>> {
        let p = problem.and_then(|p| p.downcast_ref::<Value>()).cloned().unwrap_or_else(|| json!({}));
        Some(Self::declaration(&p).map(|d| d.to_value()))
    }

    fn coupling_validation(
        &self,
        problem: Option<&ProviderProblem>,
        for_optimization: bool,
    ) -> Option<Result<Value, String>> {
        let p = problem.and_then(|p| p.downcast_ref::<Value>())?;
        let normalized = match normalise(p) {
            Ok(n) => n,
            Err(e) => return Some(Err(e.message().to_string())),
        };
        let declaration = match Self::declaration(&normalized) {
            Ok(d) => d,
            Err(e) => return Some(Err(e.message().to_string())),
        };
        let rules = implexity_core::registries::global().extensions.coupling_rules();
        Some(Ok(implexity_core::coupling_graph::validate_declaration(&declaration, &rules, for_optimization)))
    }

    fn runtime_support(&self) -> Option<Map<String, Value>> {
        Some(crate::util::obj(
            json!({"status": "native_field_solver", "history": true, "data": "user_required", "limitations": LIMITATIONS}),
        ))
    }

    fn component_slots(&self) -> Option<Map<String, Value>> {
        Some(super::component_slots())
    }

    fn authoring_contract(&self) -> Option<Map<String, Value>> {
        Some(super::authoring_contract())
    }

    fn interface(&self, name: &str) -> Option<&(dyn Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(name)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl DesignOperations for NativeSolidHistoryProvider {
    fn provides(&self, op: DesignOp) -> bool {
        matches!(
            op,
            DesignOp::EvaluateDesign
                | DesignOp::PreflightDesign
                | DesignOp::SensitivityDesign
                | DesignOp::SensitivitiesDesign
                | DesignOp::AcceptDesign
        )
    }

    fn evaluate_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        _operating_point: usize,
    ) -> Result<Evaluation, CaeError> {
        self.evaluate_named(problem_value(problem)?, design)
    }

    fn preflight_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> Result<Map<String, Value>, CaeError> {
        let (p, k, _) = self.parts(problem_value(problem)?, design)?;
        Ok(crate::util::obj(json!({"ok": true, "issues": [], "components": p["components"],
            "state_unknowns_per_step": k.state_size, "physical_qualification": false, "limitations": LIMITATIONS})))
    }

    fn sensitivity_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        response: &str,
        _operating_point: usize,
    ) -> Result<DesignSensitivity, CaeError> {
        let mut out = self.sensitivities_named(problem_value(problem)?, design, &[response.to_string()])?;
        Ok(DesignSensitivity {
            value: out.responses.get(response).copied().unwrap_or(f64::NAN),
            gradients: out.gradients.remove(response).unwrap_or_default(),
            diagnostics: out.diagnostics,
        })
    }

    fn sensitivities_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        responses: &[String],
        _operating_point: usize,
    ) -> Result<DesignSensitivities, CaeError> {
        self.sensitivities_named(problem_value(problem)?, design, responses)
    }

    fn accept_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> Result<Option<Map<String, Value>>, CaeError> {
        self.parts(problem_value(problem)?, design)?;
        Ok(Some(crate::util::obj(json!({"design_state_id": design_identity(design)?}))))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SolidHistoryFactory;

impl SolidHistoryFactory {

    pub fn validate(p: &Value) -> Result<Value, CaeError> {
        if p.get("structural_dynamics").is_some_and(|v| !v.is_null()) {
            return contract(
                "structural_dynamics is supported by native_solid_history only; composite hosts have no qualified inertial interface coupling",
            );
        }
        normalise(p)
    }


    pub fn create(p: &Value) -> Result<Arc<SolidKernel>, CaeError> {
        Ok(Arc::new(SolidKernel::new(Self::validate(p)?)?))
    }
}


pub fn register_history_component(ctx: &InstallContext<'_>) -> Result<AddInContract, CaeError> {
    let strings = |items: &[&str]| items.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    register_strict(
        ctx,
        crate::polymer::COMPONENT_ID,
        SolidComponent::Maxwell,
        "viscoelastic_solid",
        AddInCategory::Constitutive,
        "solid",
        strings(&crate::polymer::LIMITATIONS),
    )?;
    register_strict(
        ctx,
        crate::fatigue::COMPONENT_ID,
        SolidComponent::Fatigue,
        "fatigue_history_observer",
        AddInCategory::Constitutive,
        "solid",
        strings(&["Postprocess usage only; no objective gradient or stiffness feedback."]),
    )?;
    register_strict(
        ctx,
        "inelastic_solid_history_block",
        SolidComponent::HistoryBlock,
        "solid_history_field",
        AddInCategory::Field,
        "solid",
        strings(&LIMITATIONS),
    )
}
