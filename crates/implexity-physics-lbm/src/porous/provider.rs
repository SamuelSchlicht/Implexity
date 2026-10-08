// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use implexity_ad::{Dual, Scalar};
use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, ProviderCapabilities, ProviderDescriptor, ProviderProblem,
    Sensitivity,
};
use implexity_core::coupling_graph::{CouplingDeclaration, CouplingEdge};
use implexity_core::orchestration::{
    AddInCategory, AddInContract, DesignCoordinateRef, ExecutionKind, Fidelity, PublishedContract,
    ResponseCapability, RuntimeRoute,
};
use implexity_core::{CaeError, CaeResult};
use implexity_geometry::field_registration::axis_aligned_registration;
use implexity_linalg::dense::DenseMatrix;
use implexity_optim::optimizer::OptimizerLifecycleConfig;
use implexity_optim::provider_ops::{CachedEvaluation, DesignOp, DesignOperations, LifecycleDeclaration};
use implexity_solve::matrix::{FnAction, Jacobian};
use implexity_solve::native_history::{
    HistoryOptions, HistoryProblem, HistorySolution, HistorySolveOptions, NativeHistorySystem,
};
use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use super::authoring::{Binding, build, normalise};
use super::catalog::{self, FIELD_NAMES, LIMITATIONS, LOCAL};
use super::history::{self, linearize, observe};
use super::lattice::{C, CS2, Q, exchange, quadrature_and_gradient};
use super::owner::Owner;
use super::sp::Sp;
use super::viscous::initialization_metadata;
use crate::design_ops::{Design, DesignSensitivities, DesignSensitivity, array};
use crate::rv::Recording;

pub const NAME: &str = "native_porous_d3q27_history";
pub const IMPLEMENTATION: &str = "implexity.lbm.porous_provider.PorousNativeProvider";
pub const COORDS: [&str; 2] = ["model:rho", "model:c"];

fn err(msg: impl Into<String>) -> CaeError {
    CaeError::contract(msg.into())
}

fn field(values: Vec<f64>, shape: &[usize]) -> CaeResult<FieldValue> {
    Ok(FieldValue::Array(array(values, shape)?))
}

struct PorousHistory {
    owner: Arc<Owner>,
    cache: Mutex<Option<(Vec<u64>, Arc<[Sp; 3]>)>>,
}

impl PorousHistory {
    fn matrices(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Arc<[Sp; 3]>> {
        let mut key: Vec<u64> = vec![n as u64];
        key.extend(z.iter().chain(old).chain(x).map(|v| v.to_bits()));
        if let Ok(guard) = self.cache.lock()
            && let Some((k, m)) = guard.as_ref()
            && *k == key
        {
            return Ok(Arc::clone(m));
        }
        let m = Arc::new(self.owner.full_partials(n, z, old, x)?);
        if let Ok(mut guard) = self.cache.lock() {
            *guard = Some((key, Arc::clone(&m)));
        }
        Ok(m)
    }
}

impl HistoryProblem for PorousHistory {
    fn residual(&self, n: usize, z: &[f64], prev: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        let o = &self.owner;
        o.validate_state(n, z, prev, x)?;
        o.native_state(n, z, prev, x)?;
        o.native_state(n - 1, prev, prev, x)?;
        o.residual(n, z, prev, x)
    }

    fn state_jacobian(&self, n: usize, z: &[f64], prev: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.matrices(n, z, prev, x)?[0].clone()))
    }

    fn previous_jacobian(&self, n: usize, z: &[f64], prev: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.matrices(n, z, prev, x)?[1].clone()))
    }

    fn design_jacobian(&self, n: usize, z: &[f64], prev: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.matrices(n, z, prev, x)?[2].clone()))
    }
}

pub struct Solved {
    pub p: Value,
    pub b: Binding,
    pub system: NativeHistorySystem,
    pub solution: HistorySolution,
    summary: Mutex<Option<(Vec<String>, (Map<String, Value>, Value))>>,
}

impl Solved {
    fn owner(&self) -> &Owner {
        &self.b.owner
    }

    fn states(&self) -> &[Vec<f64>] {
        &self.solution.states
    }

    fn history_observation(&self, compute: bool) -> CaeResult<Option<(Map<String, Value>, Value)>> {
        let names = history::selected(&self.p);
        if names.is_empty() {
            return Ok(Some((Map::new(), Value::Null)));
        }
        if let Ok(guard) = self.summary.lock()
            && let Some((cached, result)) = guard.as_ref()
            && *cached == names
        {
            return Ok(Some(result.clone()));
        }
        if !compute {
            return Ok(None);
        }
        let result = observe(self.owner(), self.states(), &self.b.design, &names)?;
        if let Ok(mut guard) = self.summary.lock() {
            *guard = Some((names, result.clone()));
        }
        Ok(Some(result))
    }
}

pub struct PorousNativeProvider {
    batch: usize,
    last: Mutex<Option<(String, Arc<Solved>)>>,
}

impl std::fmt::Debug for PorousNativeProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PorousNativeProvider").field("batch", &self.batch).finish_non_exhaustive()
    }
}

impl Default for PorousNativeProvider {
    fn default() -> Self {
        Self { batch: 8, last: Mutex::new(None) }
    }
}

fn design_value(design: &Design) -> Value {
    let mut m = Map::new();
    for (k, v) in design.iter() {
        m.insert(k.to_string(), crate::nparray::to_nested(&v.iter().copied().collect::<Vec<_>>(), v.shape()));
    }
    Value::Object(m)
}

fn cache_key(problem: &Value, design: &Design) -> String {
    let doc = json!({"problem": problem, "design": design_value(design)});
    implexity_core::wire::fingerprint_value(&doc)
}

impl PorousNativeProvider {

    pub fn new(batch: usize) -> CaeResult<Self> {
        if batch == 0 {
            return Err(err("positive integer derivative_batch_size required"));
        }
        Ok(Self { batch, last: Mutex::new(None) })
    }


    pub fn normalise_problem(&self, problem: &Value) -> CaeResult<Value> {
        normalise(problem)
    }


    pub fn validate_responses(&self, problem: &Value, names: &[String]) -> CaeResult<()> {
        let p = normalise(problem)?;
        let missing = catalog::missing_local(p.get("reference_wall").filter(|v| !v.is_null()));
        let selected = history::selected(&p);
        let mut absent: Vec<String> = names
            .iter()
            .filter(|n| {
                !(LOCAL.iter().any(|r| r.0 == n.as_str()) && !missing.contains(&n.as_str())
                    || selected.contains(n))
            })
            .cloned()
            .collect();
        if !absent.is_empty() {
            absent.sort();
            absent.dedup();
            let listed: Vec<String> = absent.iter().map(|v| format!("'{v}'")).collect();
            return Err(err(format!(
                "select requested history responses explicitly in problem.history_responses: [{}]",
                listed.join(", ")
            )));
        }
        Ok(())
    }


    pub fn prepare(&self, problem: &Value, design: &Design) -> CaeResult<(Value, Binding)> {
        let p = normalise(problem)?;
        let names = design.names();
        if names.len() != 2 || !COORDS.iter().all(|c| design.contains(c)) {
            return Err(err("exact model:rho/model:c channels required"));
        }
        let grid: Vec<usize> = p["solid"]["grid"]
            .as_array()
            .map(|g| g.iter().filter_map(|v| v.as_u64().and_then(|v| usize::try_from(v).ok())).collect())
            .unwrap_or_default();
        let get = |k: &str| -> CaeResult<Vec<f64>> {
            let a = design.get(k).ok_or_else(|| err("exact model:rho/model:c channels required"))?;
            if a.shape() != grid.as_slice() {
                return Err(err("exact-grid raw channels and fixed mask required"));
            }
            Ok(a.iter().copied().collect())
        };
        let b = build(&p, &get("model:rho")?, &get("model:c")?, self.batch)?;
        Ok((p, b))
    }

    fn bind_system(
        &self,
        problem: &Value,
        design: &Design,
        relax: f64,
    ) -> CaeResult<(Value, Binding, NativeHistorySystem)> {
        let (p, b) = self.prepare(problem, design)?;
        let numerics = &p["solid"]["numerics"];
        let options = HistoryOptions {
            tolerance: relax * numerics["tolerance"].as_f64().unwrap_or(f64::NAN),
            max_iterations: numerics["max_iterations"]
                .as_u64()
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(0),
            ..HistoryOptions::default()
        };
        let problem_impl = Arc::new(PorousHistory { owner: Arc::clone(&b.owner), cache: Mutex::new(None) });
        let system = NativeHistorySystem::new(problem_impl, options)?;
        Ok((p, b, system))
    }


    pub fn solve(&self, problem: &Value, design: &Design) -> CaeResult<Arc<Solved>> {
        let key = cache_key(problem, design);
        if let Ok(guard) = self.last.lock()
            && let Some((k, s)) = guard.as_ref()
            && *k == key
        {
            return Ok(Arc::clone(s));
        }
        let (p, b, system) = self.bind_system(problem, design, 1.0)?;
        let owner = Arc::clone(&b.owner);
        let x = b.design.clone();
        let initial = owner.initial(&x);
        owner.native_state(0, &initial, &initial, &x)?;
        let solution = system.solve(&x, &initial, owner.s.nt - 1, &HistorySolveOptions::default())?;
        for (n, state) in solution.states.iter().enumerate() {
            owner.native_state(n, state, &solution.states[n.max(1) - 1], &x)?;
        }
        let solved = Arc::new(Solved { p, b, system, solution, summary: Mutex::new(None) });
        if let Ok(mut guard) = self.last.lock() {
            *guard = Some((key, Arc::clone(&solved)));
        }
        Ok(solved)
    }


    pub fn replay(
        &self,
        problem: &Value,
        design: &Design,
        states: Vec<Vec<f64>>,
        accepted_norms: Vec<f64>,
    ) -> CaeResult<Arc<Solved>> {

        let (p, b, system) = self.bind_system(problem, design, 10.0)?;
        let owner = Arc::clone(&b.owner);
        let x = b.design.clone();
        if states.len() != owner.s.nt || states.iter().any(|z| z.len() != owner.state_size) {
            return Err(err("replayed history must hold every stored state of the coupled layout"));
        }
        let tolerance = p["solid"]["numerics"]["tolerance"].as_f64().unwrap_or(f64::NAN);
        let problem_impl = PorousHistory { owner: Arc::clone(&owner), cache: Mutex::new(None) };
        if accepted_norms.len() + 1 != states.len()
            || accepted_norms.iter().any(|v| v.is_nan() || *v > tolerance)
        {
            return Err(CaeError::convergence("replayed history norms do not establish convergence"));
        }
        for n in 1..states.len() {
            let r = problem_impl.residual(n, &states[n], &states[n - 1], &x)?;
            let norm = r.iter().map(|v| v * v).sum::<f64>().sqrt();

            if norm.is_nan() || norm > 10.0 * tolerance {
                return Err(CaeError::convergence(format!(
                    "replayed history does not satisfy the coupled residual tolerance: step {n} residual norm {norm:e}"
                )));
            }
        }
        owner.native_state(0, &states[0], &states[0], &x)?;
        let solution = HistorySolution {
            newton_iterations: vec![0; states.len() - 1],
            states,
            residual_norms: accepted_norms,
            execution_context: None,
            convergence_reports: None,
        };
        Ok(Arc::new(Solved { p, b, system, solution, summary: Mutex::new(None) }))
    }


    pub fn preflight_value(&self, problem: &Value) -> CaeResult<Map<String, Value>> {
        normalise(problem)?;
        let mut m = Map::new();
        m.insert("ok".into(), json!(true));
        m.insert("provider".into(), json!(NAME));
        m.insert("scope".into(), json!("declaration_only"));
        m.insert("design_validated".into(), json!(false));
        m.insert("solve_started".into(), json!(false));
        m.insert("qualified".into(), json!(false));
        m.insert("effects".into(), json!("validates authored problem; does not construct the native kernel"));
        Ok(m)
    }


    pub fn preflight_design_value(&self, problem: &Value, design: &Design) -> CaeResult<Map<String, Value>> {
        let (p, b) = self.prepare(problem, design)?;
        let o = &b.owner;
        let x = &b.design;
        let initial = o.initial(x);
        o.validate_state(1, &initial, &initial, x)?;
        o.native_state(0, &initial, &initial, x)?;
        let mut m = Map::new();
        m.insert("ok".into(), json!(true));
        m.insert("provider".into(), json!(NAME));
        m.insert("state_size".into(), json!(o.state_size));
        m.insert("history_states".into(), json!(o.s.nt));
        m.insert(
            "flow_initialization".into(),
            Value::Object(initialization_metadata(p.get("flow_initialization").filter(|v| !v.is_null()))),
        );
        m.insert("geometry".into(), Value::Object(b.geometry_receipt.clone()));
        m.insert("design_validated".into(), json!(true));
        m.insert("qualified".into(), json!(false));
        m.insert(
            "effects".into(),
            json!(
                "constructs native kernel and evaluates authored initial state; not a convergence certificate"
            ),
        );
        m.insert("limitations".into(), json!(LIMITATIONS));
        m.insert(
            "selected_viscous_coupling".into(),
            p.get("viscous_coupling").cloned().unwrap_or(Value::Null),
        );
        m.insert(
            "reference_wall".into(),
            o.wall.as_ref().map_or(Value::Null, |w| Value::Object(w.metadata())),
        );
        Ok(m)
    }

    fn observation(
        o: &Owner,
        n: usize,
        current: &[f64],
        previous: &[f64],
        x: &[f64],
    ) -> CaeResult<(Map<String, Value>, Vec<(String, FieldValue)>, Map<String, Value>, Value)> {
        let ledger = o.transport_interval(n, current, previous, x);
        let quantities = o.interval_quantities(n, current, previous, &ledger);
        let missing = catalog::missing_local(o.wall.as_ref().map(|w| &w.selection));
        let names: Vec<String> =
            LOCAL.iter().map(|r| r.0.to_string()).filter(|n| !missing.contains(&n.as_str())).collect();
        let values = o.local_values(n, current, previous, x, &names);
        if values.iter().any(|v| !v.is_finite()) {
            return Err(err("nonfinite porous response observation"));
        }
        let responses: Map<String, Value> =
            names.iter().zip(&values).map(|(k, v)| (k.clone(), json!(v))).collect();
        let grid = o.grid.n.to_vec();
        let mut fields = Vec::new();
        for (name, v) in &quantities.fields {
            fields.push((name.clone(), field(v.clone(), &grid)?));
        }
        let times = &o.s.times;
        let mut metadata = Map::new();
        for (name, unit) in FIELD_NAMES.iter().zip(["kg/s", "W", "kg/s"]) {
            metadata.insert(
                (*name).to_string(),
                json!({"units": unit, "association": "cell", "rank": "scalar",
                    "source": "same_D3Q27_open_port_and_donor_node_caloric_operator",
                    "time_association": "interval", "time_s": times[n], "interval_start_s": times[n - 1],
                    "interval_end_s": times[n],
                    "sample_convention": "previous_populations_and_donor_temperature_current_interval_viscosity_and_skeleton_velocity"}),
            );
        }
        let update = |m: &mut Map<String, Value>, key: &str, extra: Value| {
            if let (Some(Value::Object(row)), Value::Object(e)) = (m.get_mut(key), extra) {
                for (k, v) in e {
                    row.insert(k, v);
                }
            }
        };
        update(
            &mut metadata,
            FIELD_NAMES[0],
            json!({"sign": "positive_into_domain", "support": "net pressure-port exchange; interior cells zero"}),
        );
        update(
            &mut metadata,
            FIELD_NAMES[1],
            json!({"sign": "positive_into_domain", "enthalpy_reference_temperature_K": 0.0, "excludes": "pressure_and_kinetic_energy_flux"}),
        );
        update(
            &mut metadata,
            FIELD_NAMES[2],
            json!({"meaning": "local numerical population-mass residual, not a physical mass source"}),
        );
        if o.wall.is_some() {
            for (key, unit) in [
                ("interval_wall_reference_mass_rate_kg_s", "kg/s"),
                ("interval_wall_reference_caloric_power_W", "W"),
            ] {
                let mut row = metadata[FIELD_NAMES[0]].clone();
                row["units"] = json!(unit);
                row["support"] = json!("selected_wall_face");
                row["source"] = json!("reference_domain_wall_kinematics");
                row["meaning"] =
                    json!("geometric reference-domain term, not reservoir flow through a solid wall");
                metadata.insert(key.to_string(), row);
            }
        }
        let balance: Map<String, Value> =
            quantities.balance.rows.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        if quantities.balance.rows.iter().any(|(_, v)| !v.is_finite()) {
            return Err(err("nonfinite interval balance observation"));
        }
        let mut diagnostic = json!({"schema": "implexity-porous-transport-balance/1",
            "interval_start_s": times[n - 1], "interval_end_s": times[n], "quantities": balance,
            "engineering_acceptance_assessed": false, "total_coupled_energy_balance_certified": false,
            "physical_qualification": false,
            "scope": "local fluid mass and caloric operator; nodal exchange need not vanish independently of the solid/heat residual",
            "pressure_work_scope": "gauge-pressure volumetric boundary work; not pump shaft power",
            "enthalpy_reference_temperature_K": 0.0});
        if let Some(w) = &o.wall {
            diagnostic["reference_wall"] = Value::Object(w.metadata());
        }
        Ok((responses, fields, metadata, diagnostic))
    }

    fn endpoint_fields(
        o: &Owner,
        t: &[f64],
        u: &[[f64; 3]],
    ) -> CaeResult<(Vec<(String, FieldValue)>, Map<String, Value>, Value)> {
        let grid = o.grid.n;
        if t.len() != o.s.nn || t.iter().chain(u.iter().flatten()).any(|v| !v.is_finite()) {
            return Err(err("finite exact native nodal endpoint arrays required"));
        }
        let time = o.s.times[o.s.nt - 1];
        let mut g3 = grid.to_vec();
        g3.push(3);
        let fields = vec![
            ("endpoint_temperature_K".to_string(), field(o.cal.cell_temperature(t), &grid)?),
            (
                "endpoint_displacement_m".to_string(),
                field(o.drag.solid_cell_velocity(u).into_iter().flatten().collect(), &g3)?,
            ),
        ];
        #[allow(clippy::cast_precision_loss)]
        let hi: [f64; 3] = std::array::from_fn(|a| grid[a] as f64 * o.h * 1000.0);
        let registration =
            axis_aligned_registration(grid, [0.0; 3], hi, "cell").map_err(|e| err(e.to_string()))?.to_wire();
        let index = o.s.nt - 1;
        let mut metadata = Map::new();
        metadata.insert(
            "endpoint_temperature_K".into(),
            json!({"units": "K", "association": "cell", "rank": "scalar",
                "source": "native_parity_Kuhn_T4_lumped_caloric_map", "time_s": time,
                "temporal_association": "final_stored_state", "time_index": index}),
        );
        metadata.insert(
            "endpoint_displacement_m".into(),
            json!({"units": "m", "association": "cell", "rank": "vector", "component_frame": "model_cartesian",
                "components": ["x", "y", "z"], "geometric_role": "displacement",
                "source": "native_trilinear_cell_center_map", "time_s": time,
                "temporal_association": "final_stored_state", "time_index": index}),
        );
        Ok((fields, metadata, registration))
    }

    fn fluid_endpoint(
        o: &Owner,
        n: usize,
        state: &[f64],
        previous: &[f64],
        x: &[f64],
    ) -> CaeResult<(Vec<(String, FieldValue)>, Map<String, Value>)> {
        if n < 1 || n >= o.s.nt {
            return Err(err("actual noninitial endpoint interval required"));
        }
        o.validate_state(n, state, previous, x)?;
        let (_, phi, halo) = o.phase(x);
        let (q, g) = quadrature_and_gradient(o.grid, &halo);
        let (_, u) = o.nodal_fields(n, state);
        let (_, uo) = o.nodal_fields(n - 1, previous);
        let beta = o.drag_beta(&phi);
        let v: Vec<[f64; 3]> =
            u.iter().zip(&uo).map(|(a, b)| std::array::from_fn(|k| (a[k] - b[k]) / o.dt)).collect();
        let us = o.drag.solid_cell_velocity(&v);
        let nc = o.nc();
        let f = &state[o.ns..];
        let (mut speed, mut gauge, mut density, mut mass) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for c in 0..nc {
            let cell = &f[c * Q..(c + 1) * Q];
            let m: f64 = cell.iter().sum();
            let p = CS2 * m / q[c];
            let momentum: [f64; 3] =
                std::array::from_fn(|a| (0..Q).map(|i| cell[i] * f64::from(C[i][a])).sum());
            let other: [f64; 3] = std::array::from_fn(|a| p * g[c][a] + 0.0);
            let usl = us[c].map(|x| x * o.dt / o.h);
            let r = exchange(m, &momentum, &other, &usl, beta[c]);
            speed.extend(r.intrinsic_velocity.map(|x| x * o.h / o.dt));
            gauge.push((m / q[c] - 1.0) * CS2 * o.rho * (o.h / o.dt).powi(2));
            density.push(o.rho * m / q[c]);
            mass.push(o.rho * o.h.powi(3) * m);
        }
        let grid = o.grid.n.to_vec();
        let mut g3 = grid.clone();
        g3.push(3);
        let all = [&speed, &gauge, &density, &mass, &q];
        if all.iter().any(|v| v.iter().any(|x| !x.is_finite())) {
            return Err(err("nonfinite reconstructed fluid endpoint"));
        }
        let fields = vec![
            ("endpoint_intrinsic_velocity_m_s".to_string(), field(speed, &g3)?),
            ("endpoint_gauge_pressure_Pa".to_string(), field(gauge, &grid)?),
            ("endpoint_intrinsic_density_kg_m3".to_string(), field(density, &grid)?),
            ("endpoint_population_mass_kg".to_string(), field(mass, &grid)?),
            ("effective_fluid_volume_fraction".to_string(), field(q, &grid)?),
        ];
        let time = o.s.times[n];
        let mut meta = Map::new();
        for (key, unit) in fields.iter().map(|f| f.0.clone()).zip(["m/s", "Pa", "kg/m^3", "kg", "1"]) {
            let mut row = json!({"units": unit, "association": "cell", "rank": "scalar", "time_s": time,
                "source": "current_population_moments_with_endpoint_drag_and_interval_skeleton_velocity",
                "support": "all admitted positive-Qphi cells; no threshold or reporting mask"});
            if key == "endpoint_intrinsic_velocity_m_s" {
                row["rank"] = json!("vector");
                row["components"] = json!(["x", "y", "z"]);
                row["component_frame"] = json!("model_cartesian");
                row["velocity_definition"] =
                    json!("intrinsic half-force reconstructed velocity; not superficial flux");
            }
            if key == "endpoint_gauge_pressure_Pa" {
                row["pressure_datum"] = json!("zero at intrinsic density equal reference_density_kg_m3");
                row["reference_density_kg_m3"] = json!(o.rho);
                row["absolute_pressure_note"] = json!(
                    "add authored mechanical reference pressure only if that datum is explicitly shared; lattice EOS positivity is separate"
                );
            }
            if key == "effective_fluid_volume_fraction" {
                row["temporal_association"] = json!("time_invariant_design_derived");
            } else {
                row["temporal_association"] = json!("final_stored_state");
                row["time_index"] = json!(n);
            }
            meta.insert(key, row);
        }
        Ok((fields, meta))
    }

    fn mechanical_observation(
        o: &Owner,
        n: usize,
        current: &[f64],
        previous: &[f64],
        x: &[f64],
    ) -> CaeResult<Value> {
        let ledger = o.transport_interval(n, current, previous, x);
        let (q, e) = o.mechanical_quantities(n, current, previous, &ledger);
        let mut reactions = Map::new();
        if let Some(tr) = &o.trace {
            let fluid: Vec<[f64; 3]> =
                ledger.drag.cells.iter().map(|c| c.drag.intrinsic_velocity.map(|v| v * o.h / o.dt)).collect();
            let dot = |f: &[[f64; 3]]| -> f64 {
                f.iter().zip(&fluid).map(|(a, b)| a[0] * b[0] + a[1] * b[1] + a[2] * b[2]).sum()
            };
            reactions
                .insert("pressure_trace".into(), json!(dot(&tr.loads(&ledger.pressure_pa).fluid_reaction_n)));
            if o.viscous_flag("trace_traction") {
                let cells = o.viscous_cells(previous, &ledger);
                let stress: Vec<[f64; 9]> = cells.iter().map(|c| c.intrinsic_stress).collect();
                reactions.insert("viscous_trace".into(), json!(dot(&tr.viscous_loads(&stress).1)));
            }
        }
        let mut quantities = Map::new();
        for (k, v) in q.rows.iter().chain(&e.rows) {
            quantities.insert(k.clone(), json!(v));
        }
        let finite = q.rows.iter().chain(&e.rows).all(|(_, v)| v.is_finite())
            && reactions.values().all(|v| v.as_f64().is_some_and(f64::is_finite));
        if !finite {
            return Err(CaeError::convergence("nonfinite mechanical work observation"));
        }
        let times = &o.s.times;
        Ok(json!({"schema": "implexity-porous-mechanical-work/1",
            "interval_start_s": times[n - 1], "interval_end_s": times[n],
            "quantities": quantities, "diagnostic_only_trace_reaction_power_W": reactions,
            "trace_reaction_inserted_into_fluid_residual": false,
            "load_velocity_convention": "actual_nodal_displacement_increment_over_interval_duration",
            "drag_velocity_convention": "same_force_corrected_velocity_as_source_collision",
            "collision_energy_scope": "local_raw_population_momenta_before_after_collision_only",
            "prescribed_displacement_work_included": true, "additional_heat_inserted": false,
            "total_coupled_energy_balance_certified": false, "moving_interface_fsi_installed": false,
            "engineering_acceptance_assessed": false,
            "reciprocal_reference_wall_installed": o.wall.is_some(),
            "reference_wall": o.wall.as_ref().map_or(Value::Null, |w| Value::Object(w.metadata())),
            "limitations": ["Trace diagnostic reactions are not assembled fluid work.",
                "The local raw-momentum identity excludes streaming, port fluxes and physical endpoint kinetic storage.",
                "Constitutive viscous heat is not certified as the exact discrete kinetic loss."]}))
    }

    fn energy_observation(o: &Owner, n: usize, current: &[f64], previous: &[f64], x: &[f64]) -> Value {
        let ledger = o.transport_interval(n, current, previous, x);
        let (values, extra) = o.energy_quantities(previous, current, &ledger);
        let mut quantities = Map::new();
        for (k, v) in values.rows.iter().chain(&extra.rows) {
            quantities.insert(k.clone(), json!(v));
        }
        let valid = values.rows.iter().chain(&extra.rows).all(|(_, v)| v.is_finite())
            && extra.get("minimum_stage_population_mass").is_some_and(|v| v > 0.0);
        let times = &o.s.times;
        json!({"schema": "implexity-porous-full-interval-energy-audit/1", "available": valid,
            "quantities": if valid { Value::Object(quantities) } else { Value::Null },
            "unavailable_reason": if valid { Value::Null } else { json!("raw kinetic moment is not evaluable at a nonpositive/nonfinite intermediate cell mass") },
            "interval_start_s": times[n - 1], "interval_end_s": times[n],
            "population_velocity": "raw_first_moment_divided_by_mass_not_half_force_endpoint_velocity",
            "caloric_storage": "sensible_enthalpy_cp_T_not_a_complete_internal_energy_law",
            "streaming_beam_moment": "lattice_second_moment_not_physical_thermal_energy",
            "stage_order": ["collision", "open_streaming", "reference_wall_return", "remaining_pressure_ports", "solution_update_defect"],
            "physical_dissipation_assumed_from_redistribution": false,
            "audit_changes_state_equations": false, "additional_heat_inserted": false,
            "moving_geometry_installed": false, "total_energy_qualified": false,
            "engineering_acceptance_performed": false,
            "unresolved_model_terms": ["thermodynamic_internal_energy_and_pressure_dilatation_closure",
                "physical_boundary_total_energy_flux_and_exterior_work",
                "discrete_viscous_kinetic_to_caloric_conversion",
                "native_solid_thermomechanical_stored_energy_and_prescribed_support_work",
                "heterogeneous_phase_storage_qualification"]})
    }

    fn endpoint_evaluation(s: &Solved, compute_history: bool) -> CaeResult<CachedEvaluation> {
        let o = s.owner();
        let p = &s.p;
        let states = s.states();
        let nt = o.s.nt;
        let state = &states[nt - 1];
        let previous = &states[nt - 2];
        let x = &s.b.design;
        let (mut responses, transport_fields, transport_metadata, balance) =
            Self::observation(o, nt - 1, state, previous, x)?;
        let Some((summary, reduction)) = s.history_observation(compute_history)? else {
            let mut m = Map::new();
            m.insert("available".into(), json!(false));
            m.insert("reason".into(), json!("history_response_summary_unavailable"));
            return Ok(CachedEvaluation::Unavailable(m));
        };
        let metadata_all = catalog::response_metadata();
        let mut descriptions = Map::new();
        for name in responses.keys() {
            if catalog::is_local(name) {
                descriptions.insert(name.clone(), metadata_all[name].clone());
            }
        }
        for name in summary.keys() {
            descriptions.insert(name.clone(), metadata_all[name].clone());
        }
        for (k, v) in &summary {
            responses.insert(k.clone(), v.clone());
        }
        let (t, u) = o.nodal_fields(nt - 1, state);
        let (mut endpoint, mut field_metadata, registration) = Self::endpoint_fields(o, &t, &u)?;
        let (fluid, fluid_metadata) = Self::fluid_endpoint(o, nt - 1, state, previous, x)?;
        endpoint.extend(fluid);
        field_metadata.extend(fluid_metadata);
        endpoint.extend(transport_fields);
        field_metadata.extend(transport_metadata);
        if let Some(vc) = &o.viscous {
            let ledger = o.transport_interval(nt - 1, state, previous, x);
            let cells = o.viscous_cells(previous, &ledger);
            let grid = o.grid.n.to_vec();
            let mut g9 = grid.clone();
            g9.push(9);
            let times = &o.s.times;
            for (key, unit, rank) in [
                ("bulk_viscous_stress_Pa", "Pa", "tensor"),
                ("intrinsic_viscous_stress_Pa", "Pa", "tensor"),
                ("strain_rate_s_inv", "1/s", "tensor"),
                ("viscous_dissipation_W", "W", "scalar"),
            ] {
                let name = format!("interval_{key}");
                let value = match key {
                    "bulk_viscous_stress_Pa" => {
                        field(cells.iter().flat_map(|c| c.bulk_stress).collect(), &g9)?
                    }
                    "intrinsic_viscous_stress_Pa" => {
                        field(cells.iter().flat_map(|c| c.intrinsic_stress).collect(), &g9)?
                    }
                    "strain_rate_s_inv" => field(cells.iter().flat_map(|c| c.strain).collect(), &g9)?,
                    _ => field(cells.iter().map(|c| c.heat).collect(), &grid)?,
                };
                endpoint.push((name.clone(), value));
                let mut row = json!({"units": unit, "association": "cell", "rank": rank,
                    "source": "forced_BGK_second_moment_hydrodynamic_closure", "time_association": "interval",
                    "time_s": times[nt - 1], "interval_start_s": times[nt - 2], "interval_end_s": times[nt - 1],
                    "sample_convention": "previous_populations_current_temperature_interval_skeleton_velocity",
                    "closure": vc["closure"], "kinetic_energy_balance_certified": false});
                if rank == "tensor" {
                    row["components"] = json!(["xx", "xy", "xz", "yx", "yy", "yz", "zx", "zy", "zz"]);
                    row["tensor_convention"] = json!("cartesian_matrix");
                    row["component_frame"] = json!("model_cartesian");
                }
                field_metadata.insert(name, row);
            }
        }
        let selected = history::selected(p);
        let work_names: Vec<&str> = catalog::WORK.iter().chain(&catalog::WALL).map(|d| d.0).collect();
        let work = if p.get("mechanical_work_diagnostics") == Some(&json!(true))
            || selected.iter().any(|n| work_names.contains(&n.as_str()))
        {
            Self::mechanical_observation(o, nt - 1, state, previous, x)?
        } else {
            Value::Null
        };
        let energy_names: Vec<String> =
            catalog::STAGES.iter().map(|(n, _)| format!("history_{n}_J")).collect();
        let mut diagnostics = Map::new();
        diagnostics.insert("field_registration".into(), registration);
        diagnostics.insert("field_metadata".into(), Value::Object(field_metadata));
        if p.get("energy_audit_diagnostics") == Some(&json!(true))
            || selected.iter().any(|n| energy_names.contains(n))
        {
            diagnostics
                .insert("energy_audit".into(), Self::energy_observation(o, nt - 1, state, previous, x));
        }
        diagnostics.insert(
            "flow_initialization".into(),
            Value::Object(initialization_metadata(p.get("flow_initialization").filter(|v| !v.is_null()))),
        );
        diagnostics.insert("mechanical_work".into(), work);
        diagnostics.insert(
            "reference_wall".into(),
            o.wall.as_ref().map_or(Value::Null, |w| Value::Object(w.metadata())),
        );
        diagnostics.insert("qualified".into(), json!(false));
        diagnostics.insert("source_profile".into(), Value::Object(s.b.geometry_receipt.clone()));
        diagnostics.insert("residual_norms".into(), json!(s.solution.residual_norms));
        diagnostics.insert("response_metadata".into(), Value::Object(descriptions));
        diagnostics.insert("transport_balance".into(), balance);
        diagnostics.insert("history_reduction".into(), reduction);
        diagnostics.insert(
            "response_reduction".into(),
            json!(
                "per-response declared temporal reduction; additional histories require explicit selection"
            ),
        );
        let responses: BTreeMap<String, f64> =
            responses.iter().map(|(k, v)| (k.clone(), v.as_f64().unwrap_or(f64::NAN))).collect();
        Ok(CachedEvaluation::Available(Evaluation {
            provider: NAME.into(),
            responses,
            diagnostics,
            fields: endpoint.into_iter().collect(),
        }))
    }


    pub fn evaluate_value(&self, problem: &Value, design: &Design) -> CaeResult<Evaluation> {
        let s = self.solve(problem, design)?;
        let CachedEvaluation::Available(mut e) = Self::endpoint_evaluation(&s, true)? else {
            return Err(err("history_response_summary_unavailable"));
        };
        let o = s.owner();
        let nt = o.s.nt;
        let nn = o.s.nn;
        let mut temperature = Vec::with_capacity(nt * nn);
        let mut displacement = Vec::with_capacity(nt * nn * 3);
        for (n, z) in s.states().iter().enumerate() {
            let (t, u) = o.nodal_fields(n, z);
            temperature.extend(t);
            displacement.extend(u.into_iter().flatten());
        }
        e.fields.insert("temperature_nodes_history_K".into(), field(temperature, &[nt, nn])?);
        e.fields.insert("displacement_nodes_history_m".into(), field(displacement, &[nt, nn, 3])?);
        e.fields.insert("times_s".into(), field(o.s.times.clone(), &[nt])?);
        Ok(e)
    }


    pub fn cached_evaluation(&self, problem: &Value, design: &Design) -> CaeResult<CachedEvaluation> {
        let key = cache_key(problem, design);
        let cached = self.last.lock().ok().and_then(|g| g.clone());
        let unavailable = |reason: &str| {
            let mut m = Map::new();
            m.insert("available".into(), json!(false));
            m.insert("reason".into(), json!(reason));
            Ok(CachedEvaluation::Unavailable(m))
        };
        match cached {
            None => unavailable("cache_missing"),
            Some((k, _)) if k != key => unavailable("identity_mismatch"),
            Some((_, s)) => Self::endpoint_evaluation(&s, false),
        }
    }


    pub fn sensitivities_value(
        &self,
        problem: &Value,
        design: &Design,
        responses: &[String],
    ) -> CaeResult<DesignSensitivities> {
        self.check_request(problem, responses)?;
        let s = self.solve(problem, design)?;
        self.sensitivities_of(&s, problem, responses)
    }

    fn check_request(&self, problem: &Value, responses: &[String]) -> CaeResult<()> {
        self.validate_responses(problem, responses)?;
        let all = catalog::responses();
        let unique: std::collections::BTreeSet<&String> = responses.iter().collect();
        if responses.is_empty()
            || unique.len() != responses.len()
            || responses.iter().any(|r| !all.contains(r))
        {
            return Err(err("unknown/empty/duplicate response request"));
        }
        Ok(())
    }


    pub fn sensitivities_of(
        &self,
        s: &Solved,
        problem: &Value,
        responses: &[String],
    ) -> CaeResult<DesignSensitivities> {
        self.check_request(problem, responses)?;
        let o = s.owner();
        let x = &s.b.design;
        let states = s.states();
        let nt = o.s.nt;
        let nz = o.state_size;
        let nd = o.design_size;
        let m = responses.len();
        let local: Vec<String> = responses.iter().filter(|r| catalog::is_local(r)).cloned().collect();
        let whole: Vec<String> = responses.iter().filter(|r| !catalog::is_local(r)).cloned().collect();
        let selected = history::selected(&s.p);
        if whole.iter().any(|w| !selected.contains(w)) {
            return Err(err("history response was not explicitly selected in the problem"));
        }
        let mut values = vec![0.0; m];
        let mut gu: Vec<DenseMatrix> = (0..nt).map(|_| DenseMatrix::zeros(nz, m)).collect();
        let mut gx = DenseMatrix::zeros(nd, m);
        if !local.is_empty() {
            let rec = Recording::start();
            let z = rec.inputs(&states[nt - 1]);
            let old = rec.inputs(&states[nt - 2]);
            let xv = rec.inputs(x);
            let out = o.local_values(nt - 1, &z, &old, &xv, &local);
            let inputs: Vec<_> = z.iter().chain(&old).chain(&xv).copied().collect();
            for (column, name) in local.iter().enumerate() {
                let target = responses.iter().position(|r| r == name).unwrap_or(0);
                values[target] = out[column].value();
                let g = rec.gradient(out[column], &inputs);
                for i in 0..nz {
                    gu[nt - 1].data[i * m + target] += g[i];
                    gu[nt - 2].data[i * m + target] += g[nz + i];
                }
                for i in 0..nd {
                    gx.data[i * m + target] += g[2 * nz + i];
                }
            }
        }
        if !whole.is_empty() {
            let (v, state_partials, design_partials) = linearize(o, states, x, &whole)?;
            let w = whole.len();
            for (column, name) in whole.iter().enumerate() {
                let target = responses.iter().position(|r| r == name).unwrap_or(0);
                values[target] = v[column];
                for (n, sp) in state_partials.iter().enumerate() {
                    for i in 0..nz {
                        gu[n].data[i * m + target] += sp.data[i * w + column];
                    }
                }
                for i in 0..nd {
                    gx.data[i * m + target] += design_partials.data[i * w + column];
                }
            }
        }
        let owner = Arc::clone(&s.b.owner);
        let owner_t = Arc::clone(&s.b.owner);
        let x_f = x.clone();
        let x_t = x.clone();
        let initial = FnAction::new(
            (nz, nd),
            move |d: &[f64]| Ok(initial_jvp(&owner, &x_f, d)),
            move |w: &[f64]| Ok(initial_vjp(&owner_t, &x_t, w)),
        );
        let out = s.system.adjoint_many(
            x,
            &s.solution,
            &gu,
            &gx,
            Some(Jacobian::Operator(Arc::new(initial))),
            None,
        )?;
        let nc = o.nc();
        let grid = o.grid.n.to_vec();
        let mut result = DesignSensitivities::default();
        for (column, name) in responses.iter().enumerate() {
            let mut raw = vec![0.0; 2 * nc];
            for (k, index) in s.b.raw_design_indices.iter().enumerate() {
                raw[*index] = out.gradients.data[k * m + column];
            }
            let mut g = Design::new();
            g.insert("model:rho", array(raw[..nc].to_vec(), &grid)?);
            g.insert("model:c", array(raw[nc..].to_vec(), &grid)?);
            result.gradients.insert(name.clone(), g);
            result.responses.insert(name.clone(), values[column]);
        }
        let d = &mut result.diagnostics;
        d.insert("adjoint_factorizations".into(), json!(out.adjoint_factorizations));
        d.insert("adjoint_factorization_builds".into(), json!(out.adjoint_factorization_builds));
        d.insert("adjoint_factorization_reuses".into(), json!(out.adjoint_factorization_reuses));
        d.insert(
            "maximum_transpose_relative_residual".into(),
            json!(out.maximum_transpose_relative_residual),
        );
        d.insert("history_states_retained".into(), json!(out.history_states_retained));
        d.insert("history_derivative".into(), json!(out.history_derivative));
        if let Some((_, reduction)) = s.history_observation(true)?
            && !reduction.is_null()
        {
            d.insert("history_reduction".into(), reduction);
            let metadata = catalog::response_metadata();
            let rows: Map<String, Value> =
                responses.iter().map(|r| (r.clone(), metadata[r].clone())).collect();
            d.insert("response_metadata".into(), Value::Object(rows));
        }
        Ok(result)
    }

    #[must_use]
    pub fn descriptor() -> ProviderDescriptor {
        let names = catalog::responses();
        let mut d =
            ProviderDescriptor::new(NAME, vec!["flow".into(), "thermal".into(), "structure".into()], names);
        let mut fields: Vec<String> = [
            "temperature_nodes_history_K",
            "displacement_nodes_history_m",
            "times_s",
            "endpoint_temperature_K",
            "endpoint_displacement_m",
            "endpoint_intrinsic_velocity_m_s",
            "endpoint_gauge_pressure_Pa",
            "endpoint_intrinsic_density_kg_m3",
            "endpoint_population_mass_kg",
            "effective_fluid_volume_fraction",
            "interval_bulk_viscous_stress_Pa",
            "interval_intrinsic_viscous_stress_Pa",
            "interval_strain_rate_s_inv",
            "interval_viscous_dissipation_W",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        fields.extend(FIELD_NAMES.iter().map(|s| (*s).to_string()));
        d.fields = fields;
        d.sensitivities = true;
        d.nonlinear = true;
        d.design_coordinates = COORDS.iter().map(|s| (*s).to_string()).collect();
        d.notes = LIMITATIONS.iter().map(|s| (*s).to_string()).collect();
        d.response_metadata = catalog::response_metadata();
        let traits = json!({"experimental": true, "qualification_claim": false,
            "geometry_profile": "common_positive_quadrature_native_phase_volumes_v1",
            "physical_grid_designable": false, "viscous_heating": false,
            "viscous_heating_selection": "explicit_opt_in_constitutive_closure",
            "history_retention": "all_states",
            "flow_initialization_selection": "problem.flow_initialization",
            "mechanical_work_diagnostics_selection": "problem.mechanical_work_diagnostics_or_work_history_responses",
            "reciprocal_moving_interface_work": false,
            "reciprocal_reference_wall_selection": "problem.reference_wall",
            "reference_wall_kinematics": "small_displacement_reference_domain",
            "finite_motion_fsi": false});
        d.traits = traits.as_object().cloned().unwrap_or_default();
        d
    }


    pub fn contract() -> CaeResult<AddInContract> {
        let inputs: Vec<DesignCoordinateRef> = COORDS
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let mut r = DesignCoordinateRef::new(*k, format!("{NAME}.design.{i}"));
                r.addin_id = NAME.to_string();
                r
            })
            .collect();
        let metadata = catalog::response_metadata();
        let mut c = AddInContract::new(NAME);
        c.category = AddInCategory::Field;
        c.responses = catalog::responses()
            .iter()
            .map(|name| {
                let mut r = ResponseCapability::new(name.clone());
                r.unit = metadata[name]["unit"].as_str().unwrap_or_default().to_string();
                r.differentiable = Some(true);
                r.design_reachable = Some(true);
                r.depends_on = inputs.iter().map(|i| i.port_id.clone()).collect();
                r
            })
            .collect();
        c.scope = vec!["flow".into(), "thermal".into(), "structure".into()];
        c.fidelity = Fidelity::Screening;
        c.priority = 50;
        c.runtime_route = RuntimeRoute::Array;
        c.exact_design_derivatives = Some(true);
        c.exact_state_transpose = Some(true);
        c.notes = LIMITATIONS.iter().map(|s| (*s).to_string()).collect();
        c.contract_version = 2;
        c.compatibility_mode = false;
        c.owner_id = format!("provider:{NAME}");
        c.execution_kind = Some(ExecutionKind::Provider);
        c.supported_operations = ["preflight_design", "evaluate", "sensitivity", "sensitivities", "optimize"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        c.no_op_operations = Vec::new();
        c.design_inputs = inputs;
        c.checked()
    }


    pub fn declaration(p: &Value) -> CaeResult<CouplingDeclaration> {
        let edge = |s: &str, t: &str, q: &str, reason: &str| {
            CouplingEdge::new(s, t, q, "monolithic", true, reason).map_err(|e| err(e.0))
        };
        let solid = &p["solid"];
        let components = &solid["components"];
        let mut edges = vec![
            edge(
                "flow",
                "thermal",
                "population_enthalpy_and_drag_heat",
                "Conserved population enthalpy transport and relative-drag heat in shared nodal caloric residual.",
            )?,
            edge(
                "thermal",
                "flow",
                "viscosity_temperature",
                "Same nodal temperature supplies pointwise authored viscosity; constant law may have zero derivative.",
            )?,
            edge(
                "thermal",
                "structure",
                "temperature_field",
                "Native shared temperature enters selected native constitutive equations.",
            )?,
            edge(
                "flow",
                "structure",
                "pressure_and_shear_load",
                "Solved pressure and equal-opposite porous drag. Optional collisional viscous trace is a separate declared edge.",
            )?,
            edge(
                "structure",
                "flow",
                "solid_velocity",
                "Native displacement interval velocity enters relative drag; no moving-grid boundary.",
            )?,
        ];
        if p.get("reference_wall").is_some_and(|v| !v.is_null()) {
            edges.push(edge("structure", "flow", "reference_wall_velocity", "Actual halfway returned populations use the same displacement-increment wall velocity as the solid force transpose. Fixed reference domain only.")?);
            edges.push(edge("flow", "structure", "reference_wall_momentum_exchange", "Population material traction with moving-mass momentum correction replaces sampled pressure and viscous trace loads.")?);
            edges.push(edge("structure", "thermal", "reference_wall_geometric_caloric_transport", "Normal reference-wall mass term transports local previous nodal cp*T, not reservoir fluid or mechanical work as heat.")?);
        }
        let reversible = components.get("material") == Some(&json!("constant_strain_thermoelastic_solid"));
        let inelastic =
            ["plasticity", "creep"].iter().any(|k| components.get(*k).is_some_and(|v| !v.is_null()))
                || solid.get("viscoelasticity").is_some_and(|v| !v.is_null());
        if reversible {
            edges.push(edge(
                "structure",
                "thermal",
                "reversible_thermoelastic_heat",
                "Native material entropy storage uses current and previous strain and temperature.",
            )?);
        }
        if inelastic {
            edges.push(edge("structure", "thermal", "inelastic_dissipation_heat", "Selected native plastic/creep/viscoelastic dissipation; independent of reversible entropy storage.")?);
        }
        let viscous = p.get("viscous_coupling").filter(|v| !v.is_null());
        if viscous.is_some_and(|v| v["heating"] == json!(true)) {
            edges.push(edge("flow", "thermal", "constitutive_viscous_heat", "Forced BGK second-moment work with current viscosity, source population mass, and shared thermal transpose.")?);
        }
        if viscous.is_some_and(|v| v["trace_traction"] == json!(true)) {
            edges.push(edge("flow", "structure", "constitutive_viscous_trace", "Same collisional intrinsic stress on the explicitly authored fixed interface; no extra fluid force.")?);
        }
        Ok(CouplingDeclaration {
            provider: NAME.into(),
            active_physics: vec!["flow".into(), "thermal".into(), "structure".into()],
            edges,
            closed_loops: vec![vec!["flow".into(), "thermal".into(), "structure".into()]],
            notes: LIMITATIONS.iter().map(|s| (*s).to_string()).collect(),
            ..CouplingDeclaration::default()
        })
    }


    pub fn validation(problem: &Value, for_optimization: bool) -> CaeResult<Value> {
        let p = normalise(problem)?;
        let required = Self::declaration(&p)?;
        let actual = Self::declaration(&p)?;
        let mut errors: Vec<Value> = Vec::new();
        let mut error = |code: &str, message: String| errors.push(json!({"code": code, "message": message}));
        if actual.provider != NAME {
            error("COUPLING_PROVIDER_MISMATCH", "Internal graph provider identity differs.".into());
        }
        let key = |e: &CouplingEdge| (e.source.clone(), e.target.clone(), e.quantity.clone());
        let expected: BTreeMap<_, _> = required.edges.iter().map(|e| (key(e), e)).collect();
        let supplied: BTreeMap<_, _> = actual.edges.iter().map(|e| (key(e), e)).collect();
        if supplied.len() != actual.edges.len() {
            error("DUPLICATE_INTERNAL_COUPLING", "Duplicate internal edge identity.".into());
        }
        let tuple = |k: &(String, String, String)| format!("('{}', '{}', '{}')", k.0, k.1, k.2);
        for k in expected.keys().filter(|k| !supplied.contains_key(*k)) {
            error("MISSING_INTERNAL_COUPLING", tuple(k));
        }
        for k in supplied.keys().filter(|k| !expected.contains_key(*k)) {
            error("UNSUPPORTED_INTERNAL_COUPLING", tuple(k));
        }
        for (k, got) in &supplied {
            if expected.contains_key(k) && (got.mode != "monolithic" || !got.required) {
                error("INTERNAL_COUPLING_NOT_CLOSED", tuple(k));
            }
        }
        if !actual.intentionally_frozen.is_empty() {
            error(
                "FROZEN_INTERNAL_COUPLING",
                "This source owner has no frozen-feedback execution profile.".into(),
            );
        }
        if !actual.closed_loops.iter().any(|l| required.active_physics.iter().all(|p| l.contains(p))) {
            error("INTERNAL_LOOP_UNDECLARED", "Shared monolithic feedback loop must be declared.".into());
        }
        if !actual.ports.is_empty() {
            error(
                "UNSUPPORTED_INTERNAL_PORTS",
                "This owner declares internal residual interactions, not external numerical port bindings."
                    .into(),
            );
        }
        Ok(json!({"schema": "implexity-physics-coupling-report/1", "provider": NAME,
            "ok": errors.is_empty(), "errors": errors,
            "warnings": [{"code": "EXPERIMENTAL_INTERNAL_COUPLING",
                "message": "Structural declaration only; selected constitutive viscous terms do not certify a finite-step kinetic-energy balance, temporal accuracy or physical qualification."}],
            "activePhysics": actual.active_physics,
            "requiredEdges": required.edges.iter().map(CouplingEdge::to_value).collect::<Vec<_>>(),
            "declaredEdges": actual.edges.iter().map(CouplingEdge::to_value).collect::<Vec<_>>(),
            "closedLoops": actual.closed_loops, "for_optimization": for_optimization,
            "physical_qualification": false,
            "admission": "source_owned_porous_shared_residual_contract",
            "limitations": LIMITATIONS}))
    }
}

fn initial_jvp(o: &Owner, x: &[f64], d: &[f64]) -> Vec<f64> {
    let xs: Vec<Dual<1>> = x.iter().zip(d).map(|(v, t)| Dual::new(*v, [*t])).collect();
    o.initial(&xs).iter().map(|v| v.eps[0]).collect()
}

fn initial_vjp(o: &Owner, x: &[f64], w: &[f64]) -> Vec<f64> {
    let rec = Recording::start();
    let xv = rec.inputs(x);
    let out = o.initial(&xv);
    rec.vjp(&out, w, &xv)
}

fn problem_value(problem: &ProviderProblem) -> CaeResult<&Value> {
    crate::design_ops::problem_value(problem)
}

impl DesignOperations for PorousNativeProvider {
    fn provides(&self, op: DesignOp) -> bool {
        matches!(
            op,
            DesignOp::PreflightDesign
                | DesignOp::EvaluateDesign
                | DesignOp::SensitivityDesign
                | DesignOp::SensitivitiesDesign
                | DesignOp::OptimizerLifecycle
        )
    }

    fn evaluate_design(
        &self,
        problem: &ProviderProblem,
        design: &Design,
        operating_point: usize,
    ) -> CaeResult<Evaluation> {
        crate::design_ops::nominal(operating_point)?;
        self.evaluate_value(problem_value(problem)?, design)
    }

    fn preflight_design(&self, problem: &ProviderProblem, design: &Design) -> CaeResult<Map<String, Value>> {
        self.preflight_design_value(problem_value(problem)?, design)
    }

    fn sensitivity_design(
        &self,
        problem: &ProviderProblem,
        design: &Design,
        response: &str,
        operating_point: usize,
    ) -> CaeResult<DesignSensitivity> {
        crate::design_ops::nominal(operating_point)?;
        let mut out = self.sensitivities_value(problem_value(problem)?, design, &[response.to_string()])?;
        let value = out.responses.get(response).copied().unwrap_or(f64::NAN);
        let gradients = out.gradients.remove(response).unwrap_or_default();
        Ok(DesignSensitivity { value, gradients, diagnostics: out.diagnostics })
    }

    fn sensitivities_design(
        &self,
        problem: &ProviderProblem,
        design: &Design,
        responses: &[String],
        operating_point: usize,
    ) -> CaeResult<DesignSensitivities> {
        crate::design_ops::nominal(operating_point)?;
        self.sensitivities_value(problem_value(problem)?, design, responses)
    }

    fn optimizer_lifecycle(&self, _problem: Option<&ProviderProblem>) -> CaeResult<LifecycleDeclaration> {
        Ok(LifecycleDeclaration::Typed(OptimizerLifecycleConfig::new(
            COORDS.iter().map(|s| (*s).to_string()).collect(),
            "sensitivity_design",
            "evaluate_design",
            None,
            None,
            false,
            false,
        )?))
    }

    fn validate_response_selection(
        &self,
        problem: &ProviderProblem,
        names: &[String],
    ) -> Option<CaeResult<()>> {
        Some(problem_value(problem).and_then(|p| self.validate_responses(p, names)))
    }

    fn preflight_effects(&self, _problem: &Value) -> Option<CaeResult<Value>> {
        Some(Ok(json!({"status": "declared", "possible_effects": [
            "geometry_evaluation", "constitutive_evaluation", "initial_equilibrium_solve"]})))
    }

    fn cached_evaluation_design(
        &self,
        problem: &ProviderProblem,
        design: &Design,
        _operating_point: usize,
    ) -> Option<CaeResult<CachedEvaluation>> {
        Some(problem_value(problem).and_then(|p| self.cached_evaluation(p, design)))
    }
}

impl CaeProvider for PorousNativeProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn implementation(&self) -> &str {
        IMPLEMENTATION
    }

    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        let d = Self::descriptor();
        d.validate()?;
        Ok(ProviderCapabilities::Descriptor(Box::new(d)))
    }

    fn orchestration_contract(&self) -> Option<CaeResult<PublishedContract>> {
        Some(Self::contract().map(|c| PublishedContract::Contract(Box::new(c))))
    }

    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        normalise(problem)?;
        Ok(Arc::new(problem.clone()))
    }

    fn preflight(
        &self,
        problem: &ProviderProblem,
        _topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        self.preflight_value(problem_value(problem)?)
    }

    fn evaluate(&self, _problem: &ProviderProblem, _topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        Err(crate::provider::missing_method("PorousNativeProvider", "evaluate"))
    }

    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        _topology: &ArrayD<f64>,
        _response: &str,
    ) -> CaeResult<Sensitivity> {
        Err(crate::provider::missing_method("PorousNativeProvider", "sensitivity"))
    }

    fn coupling_declaration(&self, problem: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        let p = match problem.map(problem_value) {
            Some(Ok(v)) => v,
            Some(Err(e)) => return Some(Err(e)),
            None => return Some(Err(err("porous coupling declaration requires a problem"))),
        };
        Some(normalise(p).and_then(|n| Self::declaration(&n)).map(|d| d.to_value()))
    }

    fn coupling_validation(
        &self,
        problem: Option<&ProviderProblem>,
        for_optimization: bool,
    ) -> Option<Result<Value, String>> {
        let p = problem.and_then(|p| p.downcast_ref::<Value>())?;
        Some(Self::validation(p, for_optimization).map_err(|e| e.message().to_string()))
    }

    fn coupling_inventory(&self, problem: Option<&ProviderProblem>) -> Option<Value> {
        let normalized = match problem.and_then(|p| p.downcast_ref::<Value>()) {
            Some(p) => Some(normalise(p).ok()?),
            None => None,
        };
        implexity_physics_solid::coupling_inventory::report(NAME, normalized.as_ref(), true).ok()
    }

    fn interface(&self, name: &str) -> Option<&(dyn Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(name)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
