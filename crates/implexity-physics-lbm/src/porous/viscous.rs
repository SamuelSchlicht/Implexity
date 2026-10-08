// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value, json};

use super::lattice::{C, CS2, Q, equilibrium, forcing};
use crate::nparray::{Kind, asarray};

pub const SCHEMA: &str = "implexity-porous-viscous-coupling/1";
pub const INIT_SCHEMA: &str = "implexity-porous-flow-initialization/1";
pub const INIT_KIND: &str = "force_consistent_fields";

fn err(msg: impl Into<String>) -> CaeError {
    CaeError::contract(msg.into())
}

fn exact_keys(v: &Value, keys: &[&str]) -> bool {
    v.as_object().is_some_and(|m| m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)))
}


pub fn normalise_selection(raw: &Value) -> CaeResult<Option<Value>> {
    if raw.is_null() {
        return Ok(None);
    }
    if !exact_keys(raw, &["schema", "closure", "heating", "trace_traction", "provenance"]) {
        return Err(err("viscous coupling requires an explicit complete declaration"));
    }
    if raw["schema"] != json!(SCHEMA) || raw["closure"] != json!("forced_bgk_second_moment_v1") {
        return Err(err("unsupported viscous coupling closure"));
    }
    if !raw["heating"].is_boolean() || !raw["trace_traction"].is_boolean() {
        return Err(err("viscous coupling selections must be booleans"));
    }
    if raw["heating"] == json!(false) && raw["trace_traction"] == json!(false) {
        return Err(err("omit viscous_coupling to select neither contribution"));
    }
    if !raw["provenance"].as_str().is_some_and(|s| !s.trim().is_empty()) {
        return Err(err("viscous closure provenance is required"));
    }
    Ok(Some(raw.clone()))
}

#[derive(Clone, Copy, Debug)]
pub struct ViscousCell<S> {
    pub bulk_stress: [S; 9],
    pub intrinsic_stress: [S; 9],
    pub strain: [S; 9],
    pub heat: S,
    pub bulk_mu: S,
}

#[must_use]
pub fn stress_and_heat<S: Scalar>(
    f: &[S],
    q: S,
    tau: S,
    u: &[S; 3],
    force: &[S; 3],
    rho: f64,
    h: f64,
    dt: f64,
) -> ViscousCell<S> {
    let mass = f.iter().fold(S::zero(), |acc, v| acc + *v);
    let eq = equilibrium(mass, u);
    let neq: Vec<S> = (0..Q).map(|i| f[i] - eq[i]).collect();
    let moment: [S; 9] = std::array::from_fn(|k| {
        let (a, b) = (k / 3, k % 3);
        (0..Q).fold(S::zero(), |acc, i| acc + neq[i] * f64::from(C[i][a]) * f64::from(C[i][b]))
    });
    let corrected: [S; 9] = std::array::from_fn(|k| {
        let (a, b) = (k / 3, k % 3);
        moment[k] + (u[a] * force[b] + force[a] * u[b]) * 0.5
    });
    let den = mass * 2.0 * CS2 * tau * dt;
    let strain: [S; 9] = std::array::from_fn(|k| -corrected[k] / den);
    let bulk_mu = mass * rho * CS2 * (tau - 0.5) * (h * h) / dt;
    let bulk: [S; 9] = std::array::from_fn(|k| bulk_mu * 2.0 * strain[k]);
    let heat = (0..9).fold(S::zero(), |acc, k| acc + bulk[k] * strain[k]) * h.powi(3);
    ViscousCell { bulk_stress: bulk, intrinsic_stress: bulk.map(|v| v / q), strain, heat, bulk_mu }
}


pub fn normalise_initialization(
    value: &Value,
    grid: [usize; 3],
    rho: f64,
    h: f64,
    dt: f64,
    pressure_reference: f64,
) -> CaeResult<Option<Value>> {
    if value.is_null() {
        return Ok(None);
    }
    let keys = [
        "schema",
        "kind",
        "gauge_pressure_Pa",
        "intrinsic_velocity_m_s",
        "skeleton_velocity_m_s",
        "provenance",
    ];
    if !exact_keys(value, &keys) {
        let mut sorted = keys.to_vec();
        sorted.sort_unstable();
        let listed: Vec<String> = sorted.iter().map(|k| format!("'{k}'")).collect();
        return Err(err(format!("flow_initialization requires exact fields [{}]", listed.join(", "))));
    }
    let mut p = value.clone();
    if p["schema"] != json!(INIT_SCHEMA) || p["kind"] != json!(INIT_KIND) {
        return Err(err("unsupported porous flow initialization"));
    }
    if !p["provenance"].as_str().is_some_and(|s| !s.trim().is_empty()) {
        return Err(err("flow initialization requires explicit provenance"));
    }
    let g = grid.to_vec();
    let mut g3 = g.clone();
    g3.push(3);
    for (key, shapes) in [
        ("gauge_pressure_Pa", vec![Vec::new(), g.clone()]),
        ("intrinsic_velocity_m_s", vec![vec![3], g3.clone()]),
        ("skeleton_velocity_m_s", vec![vec![3], g3.clone()]),
    ] {
        let a = asarray(&p[key]);
        if !matches!(a.kind, Kind::Int | Kind::Float | Kind::Bool)
            || a.kind == Kind::Bool
            || !shapes.contains(&a.shape)
            || !a.all_finite()
        {
            return Err(err(format!("{key} requires finite real uniform or exact-grid initial data")));
        }
        p[key] = crate::nparray::to_nested(&a.data, &a.shape);
    }
    let pressure = asarray(&p["gauge_pressure_Pa"]).data;
    let scale = rho * (h / dt).powi(2) * CS2;
    if pressure.iter().any(|v| pressure_reference + v <= 0.0) {
        return Err(err("initial absolute physical pressure must be positive"));
    }
    if pressure.iter().any(|v| 1.0 + v / scale <= 0.0) {
        return Err(err("initial lattice EOS density must be positive"));
    }
    Ok(Some(p))
}

fn broadcast<const K: usize>(value: &Value, nc: usize) -> Vec<[f64; K]> {
    let a = asarray(value);
    if a.data.len() == K {
        vec![std::array::from_fn(|k| a.data[k]); nc]
    } else {
        (0..nc).map(|c| std::array::from_fn(|k| a.data[c * K + k])).collect()
    }
}

#[must_use]
pub fn initial_populations<S: Scalar>(
    q: &[S],
    g: &[[S; 3]],
    beta: &[S],
    selection: &Value,
    rho: f64,
    h: f64,
    dt: f64,
) -> Vec<S> {
    let nc = q.len();
    let velocity = broadcast::<3>(&selection["intrinsic_velocity_m_s"], nc);
    let skeleton = broadcast::<3>(&selection["skeleton_velocity_m_s"], nc);
    let gauge = broadcast::<1>(&selection["gauge_pressure_Pa"], nc);
    let mut out = Vec::with_capacity(nc * Q);
    for c in 0..nc {
        let u: [S; 3] = velocity[c].map(|v| S::from_f64(v * dt / h));
        let us: [f64; 3] = skeleton[c].map(|v| v * dt / h);
        let density = 1.0 + gauge[c][0] / (rho * (h / dt).powi(2) * CS2);
        let mass = q[c] * density;
        let force: [S; 3] = std::array::from_fn(|a| g[c][a] * (CS2 * density) - beta[c] * (u[a] - us[a]));
        let eq = equilibrium(mass, &u);
        let fo = forcing(&u, &force, S::one());
        out.extend((0..Q).map(|i| eq[i] - fo[i]));
    }
    out
}

#[must_use]
pub fn initialization_metadata(selection: Option<&Value>) -> Map<String, Value> {
    let explicit = selection.is_some();
    let mut m = Map::new();
    m.insert("schema".into(), json!("implexity-porous-initialization-report/1"));
    m.insert("mode".into(), json!(if explicit { INIT_KIND } else { "legacy_stationary_reference_density" }));
    m.insert("explicitly_authored".into(), json!(explicit));
    m.insert("temperature_owner".into(), json!("native_shared_nodal_initial_state"));
    m.insert("skeleton_velocity_scope".into(), json!("initial_population_force_correction_only"));
    m.insert(
        "initial_nonequilibrium".into(),
        json!(if explicit { "zero_force_corrected_second_moment" } else { "legacy_stationary" }),
    );
    m.insert("initial_design_chain".into(), json!("existing_initial_jvp_vjp_and_history_adjoint"));
    m.insert("steady_state_certified".into(), json!(false));
    m.insert("flow_relaxation_performed".into(), json!(false));
    m.insert("moving_interface_condition_installed".into(), json!(false));
    m.insert("provenance".into(), selection.map_or(Value::Null, |s| s["provenance"].clone()));
    m
}
