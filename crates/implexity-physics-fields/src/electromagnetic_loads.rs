// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value, json};

use implexity_ad::{Dual, Scalar};
use implexity_core::contracts::FieldValue;
use implexity_core::history_field_sources::HistorySourceComponent;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_physics_solid::solid_history::SolidKernel;
use implexity_solve::coupled_history::{
    CoupledHistoryAssembly, HistoryBlock, HistoryBlockCallbacks, HistoryInterface,
};
use implexity_solve::local_assembly::{Kind, LocalResidual, LocalResidualAssembly};
use implexity_solve::matrix::Jacobian;

use crate::common::{
    assembly_options, design_incidence, err, extend_coupling, grid_of, incidence, py_int, real_array,
    times_match, zero_csr,
};
use crate::electrothermal::{
    ElectricalSettings, derived_scales, effective_conductivity, electrothermal_element, validate_settings,
};
use crate::host::{
    BoundFieldSource, BoundSource, FieldHost, FieldSourceAuthoring, ResponseVjp, SourceEnergy, block_start,
    host_of,
};
use crate::prescribed_joule::{array, strings};

pub const NAME: &str = "native_electromagnetic_loads";
pub const BLOCK: &str = "electromagnetic_potential";
pub const MU0: f64 = 4e-7 * std::f64::consts::PI;
pub const RESPONSES: [(&str, &str); 11] = [
    ("electromagnetic_force_peak_N", "N"),
    ("electromagnetic_impulse_x_N_s", "N s"),
    ("electromagnetic_impulse_y_N_s", "N s"),
    ("electromagnetic_impulse_z_N_s", "N s"),
    ("electromagnetic_torque_peak_N_m", "N m"),
    ("electromagnetic_angular_impulse_x_N_m_s", "N m s"),
    ("electromagnetic_angular_impulse_y_N_m_s", "N m s"),
    ("electromagnetic_angular_impulse_z_N_m_s", "N m s"),
    ("electromagnetic_force_density_pnorm_N_m3", "N/m^3"),
    ("electromagnetic_current_density_pnorm_A_m2", "A/m^2"),
    ("electromagnetic_joule_energy_J", "J"),
];
pub const INDUCTION: &str = "uniform_field_backward_difference_quasistatic";
pub const ENERGY: &str = "authored_heat_excludes_electromagnetic_joule_heat";
pub const NODE_ORDER: &str = "native_solid_node_order";
pub const FRAME: &str =
    "native analysis frame in m: origin at the lower-corner node, node (i,j,k) at (i h_x, j h_y, k h_z)";
pub const PNORM: &str = "normalised p-mean (sum_s w_s |a_s|^p)^(1/p) over the listed samples with equal weights summing to one (every native tetrahedron has the same volume, so equal element weights are volume weights); bounded by the true maximum and at least N^(-1/p) times it for N samples";
const EXCLUSIVE: (&str, &str) = (
    "native_resolved_electrothermal",
    "a second, independent potential solve in the same conductor: currents do not superpose in the Joule heat and its Lorentz force would be missing",
);
const JOULE_SOURCES: [&str; 1] = ["prescribed_real_field_joule_heat"];
pub const SETTINGS: [&str; 20] = [
    "grid",
    "times_s",
    "node_order",
    "magnetic_flux_density_T",
    "induction_convention",
    "induction_gauge_origin_m",
    "terminals",
    "conductivity_S_m",
    "conductivity_slope_S_m_K",
    "void_conductivity_S_m",
    "penalty",
    "joule_heating",
    "potential_scale_V",
    "current_residual_scale_A",
    "charge_tolerance_A",
    "power_tolerance_W",
    "response_norm_order",
    "moment_reference_m",
    "energy_convention",
    "provenance",
];
const DATA: &str = include_str!("data/electromagnetic_loads.json");

fn data() -> Value {
    serde_json::from_str(DATA).unwrap_or(Value::Null)
}

#[must_use]
pub fn limitations() -> Vec<String> {
    strings(&data()["limitations"])
}

fn canonical_id(s: &str) -> bool {

    let mut chars = s.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let mut previous_underscore = false;
    for c in chars {
        if c == '_' {
            if previous_underscore {
                return false;
            }
            previous_underscore = true;
        } else if c.is_ascii_alphanumeric() {
            previous_underscore = false;
        } else {
            return false;
        }
    }
    !previous_underscore
}

#[derive(Clone, Debug, PartialEq)]
pub struct Terminal {
    pub id: String,
    pub ground: bool,
    pub axis: usize,
    pub hi: bool,
    pub lower: [f64; 2],
    pub upper: [f64; 2],
    pub current_a: Vec<f64>,
}

impl Terminal {
    fn from_value(row: &Value) -> Self {
        let pair = |v: &Value| -> [f64; 2] {
            let a: Vec<f64> =
                v.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
            [a.first().copied().unwrap_or(0.0), a.get(1).copied().unwrap_or(0.0)]
        };
        Self {
            id: row["id"].as_str().unwrap_or_default().to_string(),
            ground: row["role"] == json!("ground"),
            axis: usize::try_from(row["axis"].as_u64().unwrap_or(0)).unwrap_or(0),
            hi: row["side"] == json!("hi"),
            lower: pair(&row["lower_fraction"]),
            upper: pair(&row["upper_fraction"]),
            current_a: row["current_A"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_f64).collect())
                .unwrap_or_default(),
        }
    }
}

#[must_use]
pub fn terminal_nodes(grid: [usize; 3], ijk: &[[usize; 3]], t: &Terminal) -> Vec<usize> {
    let tangential: Vec<usize> = (0..3).filter(|a| *a != t.axis).collect();
    let target = if t.hi { grid[t.axis] } else { 0 };
    ijk.iter()
        .enumerate()
        .filter(|(_, p)| {
            p[t.axis] == target
                && tangential.iter().enumerate().all(|(k, a)| {
                    let f = p[*a] as f64 / grid[*a] as f64;
                    f >= t.lower[k] - 1e-12 && f <= t.upper[k] + 1e-12
                })
        })
        .map(|(i, _)| i)
        .collect()
}

fn terminal(row: &Value, nt: usize) -> CaeResult<Value> {
    let Some(m) = row.as_object() else { return Err(err("electromagnetic terminal must be an object")) };
    let role = m.get("role");
    let mut keys = vec!["id", "role", "axis", "side", "lower_fraction", "upper_fraction"];
    if role == Some(&json!("current")) {
        keys.push("current_A");
    }
    let role_ok = role == Some(&json!("ground")) || role == Some(&json!("current"));
    if !role_ok || m.len() != keys.len() || !keys.iter().all(|k| m.contains_key(*k)) {
        return Err(err(
            "terminal requires id, role ground|current, axis, side, lower/upper fraction and current_A exactly for current terminals",
        ));
    }
    if !m["id"].as_str().is_some_and(|s| s.chars().count() <= 64 && canonical_id(s)) {
        return Err(err("terminal id must be a canonical identifier"));
    }
    if py_int(&m["axis"]).is_none_or(|a| !(0..3).contains(&a))
        || !(m["side"] == json!("lo") || m["side"] == json!("hi"))
    {
        return Err(err("terminal axis must be 0, 1 or 2 and side lo or hi"));
    }
    let lo = real_array(&m["lower_fraction"], "terminal lower fraction")?;
    let hi = real_array(&m["upper_fraction"], "terminal upper fraction")?;
    if lo.0 != [2]
        || hi.0 != [2]
        || lo.1.iter().any(|v| *v < 0.0)
        || hi.1.iter().any(|v| *v > 1.0)
        || lo.1.iter().zip(&hi.1).any(|(a, b)| a >= b)
    {
        return Err(err("terminal patch fractions require 0 <= lower < upper <= 1 on both tangential axes"));
    }
    let mut out = m.clone();
    out.insert("lower_fraction".into(), json!(lo.1));
    out.insert("upper_fraction".into(), json!(hi.1));
    if role == Some(&json!("current")) {
        let current = real_array(&m["current_A"], "terminal current")?;
        if current.0 != [nt] || current.1[0] != 0.0 {
            return Err(err(
                "terminal current requires one value per host time (A, into the domain) and zero initial current",
            ));
        }
        out.insert("current_A".into(), json!(current.1));
    }
    Ok(Value::Object(out))
}

fn point(value: &Value, label: &str) -> CaeResult<Vec<f64>> {
    let p = real_array(value, label)?;
    if p.0 != [3] {
        return Err(err(format!("{label} requires one finite [x, y, z] point in m ({FRAME})")));
    }
    Ok(p.1)
}

fn companions(settings: &Map<String, Value>, context: &Value) -> CaeResult<()> {
    let rows = context.get("field_sources");
    let Some(rows) = rows.filter(|v| !v.is_null()) else { return Ok(()) };
    let Some(rows) = rows.as_array() else { return Err(err("field_sources must be an explicit list")) };
    let names: Vec<&Value> =
        rows.iter().filter(|r| r.is_object()).map(|r| r.get("component").unwrap_or(&Value::Null)).collect();
    if names.iter().filter(|n| n.as_str() == Some(NAME)).count() > 1 {
        return Err(err(
            "at most one electromagnetic loads source per problem; superpose fields and terminals in one declaration",
        ));
    }
    if names.iter().any(|n| n.as_str() == Some(EXCLUSIVE.0)) {
        return Err(err(format!(
            "electromagnetic loads cannot be combined with {}: {}",
            EXCLUSIVE.0, EXCLUSIVE.1
        )));
    }
    if settings["joule_heating"] == json!(true)
        && names.iter().any(|n| JOULE_SOURCES.iter().any(|j| n.as_str() == Some(*j)))
    {
        return Err(err(
            "electromagnetic Joule heating would double-count the prescribed-field Joule heat source; set joule_heating false or remove that source",
        ));
    }
    Ok(())
}

fn node_ijk(grid: [usize; 3]) -> Vec<[usize; 3]> {
    let mut out = Vec::new();
    for i in 0..=grid[0] {
        for j in 0..=grid[1] {
            for k in 0..=grid[2] {
                out.push([i, j, k]);
            }
        }
    }
    out
}

fn to_list(shape: &[usize], data: &[f64]) -> Value {
    if shape.is_empty() {
        return json!(data[0]);
    }
    let inner: usize = shape[1..].iter().product();
    Value::Array((0..shape[0]).map(|i| to_list(&shape[1..], &data[i * inner..(i + 1) * inner])).collect())
}


pub fn normalise(settings: &Value, context: &Value) -> CaeResult<Value> {
    let ok = settings
        .as_object()
        .is_some_and(|m| m.len() == SETTINGS.len() && SETTINGS.iter().all(|k| m.contains_key(*k)));
    let Some(s) = settings.as_object().filter(|_| ok) else {
        return Err(err(
            "electromagnetic loads require explicit grid, times, field, gauge origin, terminals, conductivity, scales, tolerances, moment reference and conventions",
        ));
    };
    let Some(solid) = context.get("solid").filter(|v| v.is_object()) else {
        return Err(err("electromagnetic loads require a native solid context"));
    };
    let Some(grid) = grid_of(&s["grid"]).filter(|_| s["grid"] == solid["grid"]) else {
        return Err(err("electromagnetic grid must match the native solid grid"));
    };
    let times = real_array(&s["times_s"], "electromagnetic times")?;
    let expected = real_array(solid.get("times_s").unwrap_or(&Value::Null), "host times")?;
    if !times_match(&times, &expected) {
        return Err(err("electromagnetic histories require every host time, without interpolation"));
    }
    let nt = times.1.len();
    if s["node_order"] != json!(NODE_ORDER)
        || s["induction_convention"] != json!(INDUCTION)
        || s["energy_convention"] != json!(ENERGY)
    {
        return Err(err("unsupported electromagnetic node order, induction or energy convention"));
    }
    let field = &s["magnetic_flux_density_T"];
    let field_ok = field.as_object().is_some_and(|m| {
        m.len() == 2 && m.contains_key("values") && m.get("layout") == Some(&json!("uniform_per_time"))
    });
    if !field_ok {
        return Err(err("magnetic flux density requires layout uniform_per_time and values [time,3] in T"));
    }
    let b = real_array(&field["values"], "magnetic flux density")?;
    if b.0 != [nt, 3] {
        return Err(err("magnetic flux density requires one uniform vector per host time"));
    }
    if !s["joule_heating"].is_boolean() {
        return Err(err("joule_heating must be boolean"));
    }
    companions(s, context)?;
    if py_int(&s["response_norm_order"]).is_none_or(|o| !(2..=64).contains(&o) || o % 2 != 0) {
        return Err(err("response_norm_order must be an even integer between 2 and 64"));
    }
    if !s["provenance"].as_str().is_some_and(|p| !p.trim().is_empty() && p.chars().count() <= 4096) {
        return Err(err("electromagnetic loads require a nonempty data provenance"));
    }
    let Some(rows) = s["terminals"].as_array().filter(|r| r.len() <= 32) else {
        return Err(err("terminals must be an explicit list (at most 32)"));
    };
    let terminals: Vec<Value> = rows.iter().map(|r| terminal(r, nt)).collect::<CaeResult<_>>()?;
    let ids: std::collections::BTreeSet<&str> = terminals.iter().filter_map(|t| t["id"].as_str()).collect();
    if ids.len() != terminals.len() {
        return Err(err("terminal ids must be unique"));
    }
    let parsed: Vec<Terminal> = terminals.iter().map(Terminal::from_value).collect();
    if parsed.iter().filter(|t| t.ground).count() > 1 {
        return Err(err("at most one ground terminal"));
    }
    let currents: Vec<&Vec<f64>> = parsed.iter().filter(|t| !t.ground).map(|t| &t.current_a).collect();
    if !parsed.iter().any(|t| t.ground) && !currents.is_empty() {
        let scale = currents
            .iter()
            .flat_map(|c| c.iter())
            .fold(0.0_f64, |m, v| m.max(v.abs()))
            .max(f64::MIN_POSITIVE);
        if (0..nt).any(|n| currents.iter().map(|c| c[n]).sum::<f64>().abs() > 1e-12 * scale) {
            return Err(err(
                "without a ground terminal the injected terminal currents must sum to zero at every host time",
            ));
        }
    }
    let ijk = node_ijk(grid);
    let mut used = std::collections::BTreeSet::new();
    for t in &parsed {
        let nodes = terminal_nodes(grid, &ijk, t);
        if nodes.is_empty() {
            return Err(err(format!("terminal {} selects no native node", t.id)));
        }
        if nodes.iter().any(|n| used.contains(n)) {
            return Err(err("terminal patches must not share native nodes"));
        }
        used.extend(nodes);
    }
    let induced =
        b.1.chunks(3)
            .collect::<Vec<_>>()
            .windows(2)
            .any(|w| w[1].iter().zip(w[0]).any(|(x, y)| x - y != 0.0));
    let gauge = if s["induction_gauge_origin_m"].is_null() {
        if !parsed.is_empty() && induced {
            return Err(err(format!(
                "induction_gauge_origin_m is required when terminals and a changing magnetic field coexist: the equipotential terminals cannot absorb the uniform field a different origin adds. Author the point to which the external circuit leads run from every terminal ({FRAME})"
            )));
        }
        Value::Null
    } else {
        json!(point(&s["induction_gauge_origin_m"], "induction_gauge_origin_m")?)
    };
    let moment = point(&s["moment_reference_m"], "moment_reference_m")?;
    let mut controls = Map::new();
    for key in [
        "conductivity_S_m",
        "conductivity_slope_S_m_K",
        "void_conductivity_S_m",
        "penalty",
        "potential_scale_V",
        "current_residual_scale_A",
    ] {
        let (shape, values) = real_array(&s[key], key)?;
        controls.insert(key.into(), to_list(&shape, &values));
    }
    let [tref, tscale, thermal] = derived_scales(solid);
    let mut merged = json!({"temperature_reference_K": tref, "temperature_scale_K": tscale, "thermal_residual_scale_W": thermal});
    for (k, v) in &controls {
        merged[k] = v.clone();
    }
    let materials = solid["materials"].as_array().cloned().unwrap_or_default();
    let lo = materials.iter().filter_map(|m| m["T_min"].as_f64()).fold(f64::NEG_INFINITY, f64::max);
    let hi = materials.iter().filter_map(|m| m["T_max"].as_f64()).fold(f64::INFINITY, f64::min);
    validate_settings(&merged, Some([lo, hi]))?;
    for key in ["charge_tolerance_A", "power_tolerance_W"] {
        let (shape, values) = real_array(&s[key], key)?;
        if !shape.is_empty() || values[0] <= 0.0 {
            return Err(err("positive scalar SI electromagnetic tolerances required"));
        }
        controls.insert(key.into(), json!(values[0]));
    }
    let mut out = s.clone();
    for (k, v) in controls {
        out.insert(k, v);
    }
    out.insert("grid".into(), json!(grid));
    out.insert("times_s".into(), json!(times.1));
    out.insert(
        "magnetic_flux_density_T".into(),
        json!({"layout": "uniform_per_time", "values": to_list(&b.0, &b.1)}),
    );
    out.insert("terminals".into(), json!(terminals));
    out.insert("induction_gauge_origin_m".into(), gauge);
    out.insert("moment_reference_m".into(), json!(moment));
    Ok(Value::Object(out))
}

#[derive(Clone, Copy, Debug)]
struct StepField {
    bdot: [f64; 3],
    b: [f64; 3],
}

#[derive(Clone, Copy, Debug)]
pub struct EmTerms<S> {
    pub charge_a: [S; 4],
    pub current_density: [S; 3],
    pub joule_w: S,
    pub force_n: [S; 3],
    pub induction_w: S,
    pub conductivity: S,
}

fn cross<S: Scalar>(a: &[S; 3], b: &[S; 3]) -> [S; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

struct EmElement {
    law: ElectricalSettings,
    grad0: Arc<Vec<[[f64; 3]; 4]>>,
    centroid: Arc<Vec<[f64; 3]>>,
    gauge_fixed: [f64; 3],
    gauge_cells: [f64; 3],
    t0: f64,
    ts: f64,
    ps: f64,
    cs: f64,
    thermal_scale: f64,
    force_scale: f64,
    heating: bool,
}

impl EmElement {
    fn terms<S: Scalar>(&self, item: usize, current: &[S], design: &[S], field: StepField) -> EmTerms<S> {
        let temperatures: [S; 4] =
            std::array::from_fn(|i| S::from_f64(self.t0) + S::from_f64(self.ts) * current[i]);
        let phi: [S; 4] = std::array::from_fn(|i| S::from_f64(self.ps) * current[16 + i]);
        let h: [S; 3] = std::array::from_fn(|a| design[1 + a] * S::from_f64(1e-3));
        let sigma = effective_conductivity(&self.law, &temperatures, design);
        let c = self.centroid[item];
        let rel: [S; 3] = std::array::from_fn(|a| {
            S::from_f64(c[a]) * h[a]
                - (S::from_f64(self.gauge_fixed[a]) + S::from_f64(self.gauge_cells[a]) * h[a])
        });
        let bdot: [S; 3] = field.bdot.map(S::from_f64);
        let impressed = cross(&bdot, &rel).map(|v| v * S::from_f64(-0.5));
        let volume = h[0] * h[1] * h[2] / S::from_f64(6.0);
        let g = &self.grad0[item];
        let gradients: [[S; 3]; 4] =
            std::array::from_fn(|i| std::array::from_fn(|a| S::from_f64(g[i][a]) / h[a]));
        let e = electrothermal_element(&phi, sigma, &gradients, volume, Some(impressed));
        let b: [S; 3] = field.b.map(S::from_f64);
        let force = cross(&e.current_density, &b).map(|v| volume * v);
        let induction = volume
            * (e.current_density[0] * impressed[0]
                + e.current_density[1] * impressed[1]
                + e.current_density[2] * impressed[2]);
        EmTerms {
            charge_a: e.potential_residual,
            current_density: e.current_density,
            joule_w: e.joule_power,
            force_n: force,
            induction_w: induction,
            conductivity: sigma,
        }
    }
}

struct EmKernel(Arc<EmElement>);

impl LocalResidual for EmKernel {
    fn residual<S: Scalar>(&self, item: usize, current: &[S], _previous: &[S], design: &[S], out: &mut [S]) {
        let field = StepField {
            bdot: [current[20].value(), current[21].value(), current[22].value()],
            b: [current[23].value(), current[24].value(), current[25].value()],
        };
        let e = &self.0;
        let t = e.terms(item, current, design, field);
        let heat = if e.heating { t.joule_w } else { S::zero() };
        let thermal = -(heat / S::from_f64(4.0)) / S::from_f64(e.thermal_scale);
        for v in out.iter_mut().take(4) {
            *v = thermal;
        }
        for node in 0..4 {
            for a in 0..3 {
                out[4 + 3 * node + a] = -(t.force_n[a] / S::from_f64(4.0)) / S::from_f64(e.force_scale);
            }
        }
        for i in 0..4 {
            out[16 + i] = t.charge_a[i] / S::from_f64(e.cs);
        }
    }
}

pub struct ElectromagneticResidual {
    element: Arc<EmElement>,
    local: LocalResidualAssembly<EmKernel>,
    incidence: Vec<i64>,
    design: Vec<i64>,
    prescribed: Vec<Vec<f64>>,
    fields: Vec<StepField>,
    ne: usize,
}

impl ElectromagneticResidual {
    fn template(&self, n: usize) -> Vec<f64> {
        let f = self.fields[n];
        (0..self.ne)
            .flat_map(|e| {
                self.prescribed[n][20 * e..20 * e + 20]
                    .iter()
                    .copied()
                    .chain(f.bdot)
                    .chain(f.b)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn previous_template(&self, n: usize) -> Vec<f64> {
        let f = self.fields[n];
        let m = if n == 0 { 0 } else { n - 1 };
        (0..self.ne)
            .flat_map(|e| {
                self.prescribed[m][20 * e..20 * e + 20]
                    .iter()
                    .copied()
                    .chain(f.bdot)
                    .chain(f.b)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn local_inputs(&self, n: usize, z: &[f64], x: &[f64], e: usize) -> ([f64; 20], [f64; 5]) {
        let current: [f64; 20] = std::array::from_fn(|k| {
            usize::try_from(self.incidence[26 * e + k]).map_or(self.prescribed[n][20 * e + k], |i| z[i])
        });
        let design: [f64; 5] =
            std::array::from_fn(|k| x[usize::try_from(self.design[5 * e + k]).unwrap_or(0)]);
        (current, design)
    }

    fn terms(&self, n: usize, z: &[f64], x: &[f64]) -> Vec<EmTerms<f64>> {
        (0..self.ne)
            .map(|e| {
                let (c, d) = self.local_inputs(n, z, x, e);
                self.element.terms(e, &c, &d, self.fields[n])
            })
            .collect()
    }
}

impl HistoryInterface for ElectromagneticResidual {
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        self.local.residual(z, old, x, &self.template(n), &self.previous_template(n))
    }

    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.local.jacobian(
            kind,
            z,
            old,
            x,
            &self.template(n),
            &self.previous_template(n),
        )?))
    }

    fn current_action(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        v: &[f64],
        transpose: bool,
    ) -> Option<CaeResult<Vec<f64>>> {
        Some(self.local.current_action(
            z,
            old,
            x,
            &self.template(n),
            &self.previous_template(n),
            v,
            transpose,
        ))
    }
}

struct PotentialBlock {
    size: usize,
    potentials: usize,
    nf: usize,
    gauge: bool,
    currents: Vec<Vec<f64>>,
    cs: f64,
    width: usize,
    g: CsrMatrix,
}

impl HistoryBlockCallbacks for PotentialBlock {
    fn residual(&self, n: usize, z: &[f64], _old: &[f64], _x: &[f64]) -> CaeResult<Vec<f64>> {
        let mut r = vec![0.0; self.size];
        for (k, i) in self.currents[n].iter().enumerate() {
            r[self.nf + k] -= i / self.cs;
        }
        if self.gauge {
            for v in r.iter_mut().take(self.potentials) {
                *v += z[self.potentials];
            }
            r[self.potentials] = z[..self.potentials].iter().sum();
        }
        Ok(r)
    }
    fn current_jacobian(&self, _n: usize, _z: &[f64], _old: &[f64], _x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.g.clone()))
    }
    fn previous_jacobian(&self, _n: usize, _z: &[f64], _old: &[f64], _x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(zero_csr(self.size, self.size)))
    }
    fn design_jacobian(&self, _n: usize, _z: &[f64], _old: &[f64], _x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(zero_csr(self.size, self.width)))
    }
}

pub struct BoundElectromagneticLoads {
    p: Value,
    solid: Arc<SolidKernel>,
    law: ElectricalSettings,
    b: Vec<[f64; 3]>,
    bdot: Vec<[f64; 3]>,
    dt: Vec<f64>,
    terminals: Vec<Terminal>,
    ground: Vec<usize>,
    free: Vec<usize>,
    pmap: Vec<i64>,
    current_terminals: Vec<Terminal>,
    current_nodes: Vec<Vec<usize>>,
    currents: Vec<Vec<f64>>,
    gauge: bool,
    centroid: Arc<Vec<[f64; 3]>>,
    gauge_fixed: [f64; 3],
    gauge_cells: [f64; 3],
    moment_reference: [f64; 3],
    contact: Vec<(Vec<usize>, Vec<f64>)>,
    contact_threshold: f64,
    heating: bool,
    order: i64,
    block: HistoryBlock,
    exchange: OnceLock<(Arc<ElectromagneticResidual>, (usize, usize))>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EmQuantities {
    pub charge_a: Vec<f64>,
    pub potential_v: Vec<f64>,
    pub terminal_current_a: Vec<f64>,
    pub terminal_potential_v: Vec<f64>,
    pub ground_current_a: f64,
    pub gauge_multiplier_a: f64,
    pub force_n: [f64; 3],
    pub torque_n_m: [f64; 3],
    pub joule_power_w: f64,
    pub induction_power_w: f64,
    pub terminal_power_w: f64,
    pub free_charge_power_defect_w: f64,
    pub power_identity_error_w: f64,
    pub conductivity_margin_s_m: f64,
}

impl BoundElectromagneticLoads {

    #[allow(clippy::cast_possible_wrap)]
    pub fn new(p: Value, host: &dyn FieldHost) -> CaeResult<Self> {
        let s = Arc::clone(host.solid());
        let m = &s.model;
        let mut merged = json!({"temperature_reference_K": m.t0, "temperature_scale_K": m.ts, "thermal_residual_scale_W": m.ks * m.ts * m.ls});
        for key in [
            "conductivity_S_m",
            "conductivity_slope_S_m_K",
            "void_conductivity_S_m",
            "penalty",
            "potential_scale_V",
            "current_residual_scale_A",
        ] {
            merged[key] = p[key].clone();
        }
        let law = validate_settings(&merged, Some([s.t_min, s.t_max]))?;
        let nt = s.times.len();
        let b: Vec<[f64; 3]> = real_array(&p["magnetic_flux_density_T"]["values"], "magnetic flux density")?
            .1
            .chunks(3)
            .map(|c| [c[0], c[1], c[2]])
            .collect();
        let mut bdot = vec![[0.0; 3]; nt];
        for n in 1..nt {
            let dt = s.times[n] - s.times[n - 1];
            bdot[n] = std::array::from_fn(|a| (b[n][a] - b[n - 1][a]) / dt);
        }
        let mut dt = vec![0.0];
        dt.extend(s.times.windows(2).map(|w| w[1] - w[0]));
        let terminals: Vec<Terminal> = p["terminals"]
            .as_array()
            .map(|a| a.iter().map(Terminal::from_value).collect())
            .unwrap_or_default();
        let mut pmap = vec![-2_i64; s.nn];
        let mut ground = Vec::new();
        let mut current_rows = Vec::new();
        let mut node_sets = Vec::new();
        for t in &terminals {
            let nodes = terminal_nodes(s.grid, &s.mesh.ijk, t);
            node_sets.push(nodes.clone());
            if t.ground {
                for n in &nodes {
                    pmap[*n] = -1;
                }
                ground = nodes;
            } else {
                for n in &nodes {
                    pmap[*n] = -3;
                }
                current_rows.push((t.clone(), nodes));
            }
        }
        let free: Vec<usize> = (0..s.nn).filter(|k| pmap[*k] == -2).collect();
        let nf = free.len();
        for (k, node) in free.iter().enumerate() {
            pmap[*node] = k as i64;
        }
        let nk = current_rows.len();
        for (k, (_, nodes)) in current_rows.iter().enumerate() {
            for n in nodes {
                pmap[*n] = (nf + k) as i64;
            }
        }
        let currents: Vec<Vec<f64>> =
            (0..nt).map(|n| current_rows.iter().map(|(t, _)| t.current_a[n]).collect()).collect();
        let gauge = ground.is_empty();
        let size = nf + nk + usize::from(gauge);
        let potentials = nf + nk;
        let g = if gauge {
            let mut rows = Vec::new();
            let mut cols = Vec::new();
            for i in 0..potentials {
                rows.push(i);
                cols.push(potentials);
            }
            for i in 0..potentials {
                rows.push(potentials);
                cols.push(i);
            }
            CsrMatrix::from_triplets(size, size, &rows, &cols, &vec![1.0; 2 * potentials])
                .map_err(|e| err(e.to_string()))?
        } else {
            zero_csr(size, size)
        };
        let width = 2 * s.nc + 3;
        let cs = law.current_residual_scale_a;
        let block = HistoryBlock {
            name: BLOCK.into(),
            initial: vec![0.0; size],
            design_indices: (0..width).collect(),
            callbacks: Arc::new(PotentialBlock {
                size,
                potentials,
                nf,
                gauge,
                currents: currents.clone(),
                cs,
                width,
                g,
            }),
            field: None,
        };
        let centroid: Vec<[f64; 3]> = s
            .mesh
            .tets
            .iter()
            .map(|t| std::array::from_fn(|a| t.iter().map(|n| s.mesh.ijk[*n][a] as f64).sum::<f64>() / 4.0))
            .collect();
        let gauge_value = &p["induction_gauge_origin_m"];
        let (gauge_fixed, gauge_cells) = if gauge_value.is_null() {
            ([0.0; 3], std::array::from_fn(|a| s.grid[a] as f64 / 2.0))
        } else {
            let g = real_array(gauge_value, "induction_gauge_origin_m")?.1;
            ([g[0], g[1], g[2]], [0.0; 3])
        };
        let moment = real_array(&p["moment_reference_m"], "moment_reference_m")?.1;
        let contact =
            terminals.iter().zip(&node_sets).map(|(t, nodes)| contact_cells(&s, nodes, t)).collect();
        let contact_threshold =
            host.problem()["validity"]["solid_reporting_threshold"].as_f64().ok_or_else(|| {
                err("electromagnetic loads require the host validity.solid_reporting_threshold")
            })?;
        Ok(Self {
            heating: p["joule_heating"] == json!(true),
            order: py_int(&p["response_norm_order"]).unwrap_or(8),
            p,
            law,
            b,
            bdot,
            dt,
            terminals,
            ground,
            free,
            pmap,
            current_terminals: current_rows.iter().map(|(t, _)| t.clone()).collect(),
            current_nodes: current_rows.into_iter().map(|(_, n)| n).collect(),
            currents,
            gauge,
            centroid: Arc::new(centroid),
            gauge_fixed,
            gauge_cells,
            moment_reference: [moment[0], moment[1], moment[2]],
            contact,
            contact_threshold,
            block,
            exchange: OnceLock::new(),
            solid: s,
        })
    }

    fn exchange_ref(&self) -> CaeResult<&(Arc<ElectromagneticResidual>, (usize, usize))> {
        self.exchange.get().ok_or_else(|| err("electromagnetic loads source is not attached"))
    }

    #[must_use]
    pub fn contact_occupancy(&self, x: &[f64]) -> Map<String, Value> {
        self.terminals
            .iter()
            .zip(&self.contact)
            .map(|(t, (cells, w))| {
                (t.id.clone(), json!(cells.iter().zip(w).map(|(c, wt)| wt * x[*c]).sum::<f64>()))
            })
            .collect()
    }

    fn check_contacts(&self, x: &[f64]) -> CaeResult<()> {
        for (id, v) in self.contact_occupancy(x) {
            let value = v.as_f64().unwrap_or(0.0);
            if value < self.contact_threshold {
                return Err(CaeError::convergence(format!(
                    "electromagnetic terminal {id} contacts void: mean occupancy {} under the patch is below the host solid_reporting_threshold {}; a terminal must inject current into solid material, move the patch or keep it solid",
                    implexity_core::extensions::format_g(value, 4),
                    implexity_core::extensions::format_g6(self.contact_threshold)
                )));
            }
        }
        Ok(())
    }

    fn positions(&self, x: &[f64]) -> (Vec<[f64; 3]>, [f64; 3]) {
        let nc = self.solid.nc;
        let h: [f64; 3] = std::array::from_fn(|a| x[nc + a] * 1e-3);
        (self.centroid.iter().map(|c| std::array::from_fn(|a| c[a] * h[a])).collect(), h)
    }


    pub fn quantities(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<(EmQuantities, Vec<EmTerms<f64>>)> {
        let (exchange, pl) = self.exchange_ref()?;
        let s = &self.solid;
        let t = exchange.terms(n, z, x);
        let mut charge = vec![0.0; s.nn];
        for (e, tet) in s.mesh.tets.iter().enumerate() {
            for (i, node) in tet.iter().enumerate() {
                charge[*node] += t[e].charge_a[i];
            }
        }
        let pz = &z[pl.0..pl.1];
        let ps = self.law.potential_scale_v;
        let potential: Vec<f64> =
            self.pmap.iter().map(|m| usize::try_from(*m).map_or(0.0, |k| pz[k]) * ps).collect();
        let nf = self.free.len();
        let nk = self.current_terminals.len();
        let measured: Vec<f64> =
            self.current_nodes.iter().map(|nodes| nodes.iter().map(|k| charge[*k]).sum()).collect();
        let terminal_v: Vec<f64> = pz[nf..nf + nk].iter().map(|v| v * ps).collect();
        let (r, _) = self.positions(x);
        let mut force = [0.0; 3];
        let mut torque = [0.0; 3];
        for (e, term) in t.iter().enumerate() {
            let arm: [f64; 3] = std::array::from_fn(|a| r[e][a] - self.moment_reference[a]);
            let c = cross(&arm, &term.force_n);
            for a in 0..3 {
                force[a] += term.force_n[a];
                torque[a] += c[a];
            }
        }
        let joule: f64 = t.iter().map(|v| v.joule_w).sum();
        let induction: f64 = t.iter().map(|v| v.induction_w).sum();
        let terminal_power: f64 = terminal_v.iter().zip(&measured).map(|(a, b)| a * b).sum();
        let free_power: f64 = self.free.iter().map(|k| potential[*k] * charge[*k]).sum();
        let q = EmQuantities {
            terminal_current_a: measured,
            terminal_potential_v: terminal_v,
            ground_current_a: self.ground.iter().map(|k| charge[*k]).sum(),
            gauge_multiplier_a: if self.gauge {
                pz[pz.len() - 1] * self.law.current_residual_scale_a
            } else {
                0.0
            },
            force_n: force,
            torque_n_m: torque,
            joule_power_w: joule,
            induction_power_w: induction,
            terminal_power_w: terminal_power,
            free_charge_power_defect_w: free_power,
            power_identity_error_w: joule - terminal_power - free_power - induction,
            conductivity_margin_s_m: t.iter().map(|v| v.conductivity).fold(f64::INFINITY, f64::min),
            charge_a: charge,
            potential_v: potential,
        };
        Ok((q, t))
    }

    fn screen(&self, n: usize, x: &[f64]) -> Map<String, Value> {
        let s = &self.solid;
        let h: Vec<f64> = (0..3).map(|a| x[s.nc + a] * 1e-3).collect();
        let changed = self.currents[n].iter().zip(&self.currents[n.saturating_sub(1)]).any(|(a, b)| a != b);
        let active =
            self.bdot[n].iter().any(|v| *v != 0.0) || (!self.current_terminals.is_empty() && changed);
        let law = &self.law;
        let mut sigma = law.void_conductivity;
        for t in [s.t_min, s.t_max] {
            for k in 0..2 {
                sigma = sigma.max(law.conductivity[k] + law.slope[k] * (t - s.model.t0));
            }
        }
        let l = (0..3).map(|a| s.grid[a] as f64 * h[a]).fold(f64::NEG_INFINITY, f64::max);
        let number = MU0 * sigma * l * l / self.dt[n];
        let mut m = Map::new();
        m.insert("magnetic_diffusion_number".into(), json!(number));
        m.insert("magnetic_diffusion_length_m".into(), json!(l));
        m.insert("magnetic_diffusion_conductivity_S_m".into(), json!(sigma));
        m.insert(
            "quasistatic_screen".into(),
            json!(if !active {
                "not_applicable_no_induction_or_current_change"
            } else if number <= 1.0 {
                "passed"
            } else {
                "exceeded_quasistatic_approximation_questionable"
            }),
        );
        m
    }
}

fn contact_cells(s: &SolidKernel, nodes: &[usize], t: &Terminal) -> (Vec<usize>, Vec<f64>) {
    let grid = s.grid;
    let mut members = vec![false; s.nn];
    for n in nodes {
        members[*n] = true;
    }
    let shape = [grid[0] + 1, grid[1] + 1, grid[2] + 1];
    let layer = if t.hi { grid[t.axis] - 1 } else { 0 };
    let face = usize::from(t.hi);
    let tangential: Vec<usize> = (0..3).filter(|a| *a != t.axis).collect();
    let mut cells = Vec::new();
    let mut weights = Vec::new();
    for i in 0..grid[0] {
        for j in 0..grid[1] {
            for k in 0..grid[2] {
                let c = [i, j, k];
                if c[t.axis] != layer {
                    continue;
                }
                let mut w = 0.0;
                for da in 0..2 {
                    for db in 0..2 {
                        let mut corner = c;
                        corner[t.axis] += face;
                        corner[tangential[0]] += da;
                        corner[tangential[1]] += db;
                        let id = (corner[0] * shape[1] + corner[1]) * shape[2] + corner[2];
                        if members[id] {
                            w += 1.0;
                        }
                    }
                }
                if w > 0.0 {
                    cells.push((i * grid[1] + j) * grid[2] + k);
                    weights.push(w);
                }
            }
        }
    }
    let total: f64 = weights.iter().sum();
    (cells, weights.iter().map(|w| w / total).collect())
}

fn pmean(squares: &[f64], order: i64) -> (f64, Vec<f64>) {
    let m = squares.iter().copied().fold(f64::NEG_INFINITY, f64::max).sqrt();
    if !(m > 0.0) {
        return (0.0, vec![0.0; squares.len()]);
    }
    let half = i32::try_from(order / 2).unwrap_or(1);
    let p = order as f64;
    let n = squares.len() as f64;
    let mean = squares.iter().map(|s| (s / (m * m)).powi(half)).sum::<f64>() / n;
    let value = m * mean.powf(1.0 / p);
    let factor = m * (1.0 / p) * mean.powf(1.0 / p - 1.0) / n;
    let grad =
        squares.iter().map(|s| factor * f64::from(half) * (s / (m * m)).powi(half - 1) / (m * m)).collect();
    (value, grad)
}

impl BoundFieldSource for BoundElectromagneticLoads {
    fn response_units(&self) -> Vec<(String, String)> {
        RESPONSES.iter().map(|(k, u)| ((*k).to_string(), (*u).to_string())).collect()
    }

    fn blocks(&self) -> Vec<HistoryBlock> {
        vec![self.block.clone()]
    }

    fn attach(&self, assembly: &CoupledHistoryAssembly) -> CaeResult<()> {
        let s = &self.solid;
        let sl = block_start(assembly, "solid")?;
        let pl = assembly
            .slice(BLOCK)
            .ok_or_else(|| err(format!("coupled history assembly has no {BLOCK} block")))?;
        let m = &s.model;
        let element = Arc::new(EmElement {
            law: self.law,
            grad0: Arc::new(s.mesh.gradients.clone()),
            centroid: Arc::clone(&self.centroid),
            gauge_fixed: self.gauge_fixed,
            gauge_cells: self.gauge_cells,
            t0: m.t0,
            ts: m.ts,
            ps: self.law.potential_scale_v,
            cs: self.law.current_residual_scale_a,
            thermal_scale: m.ks * m.ts * m.ls,
            force_scale: m.ss * m.ls * m.ls,
            heating: self.heating,
        });
        let so = i64::try_from(sl).unwrap_or(0);
        let po = i64::try_from(pl.0).unwrap_or(0);
        let mut inc = Vec::with_capacity(s.ne * 26);
        for tet in &s.mesh.tets {
            for n in tet {
                inc.push(if s.tmap[*n] < 0 { -1 } else { s.tmap[*n] + so });
            }
            for n in tet {
                for c in 0..3 {
                    let u = s.umap[3 * n + c];
                    inc.push(if u < 0 { -1 } else { u + so });
                }
            }
            for n in tet {
                let v = self.pmap[*n];
                inc.push(if v < 0 { -1 } else { v + po });
            }
            inc.extend([-1; 6]);
        }
        let rows: Vec<i64> = inc.chunks(26).flat_map(|c| c[..20].to_vec()).collect();
        let design = design_incidence(s);
        let local = LocalResidualAssembly::new(
            EmKernel(Arc::clone(&element)),
            incidence(s.ne, 20, rows)?,
            incidence(s.ne, 26, inc.clone())?,
            incidence(s.ne, 26, inc.clone())?,
            incidence(s.ne, 5, design.clone())?,
            assembly.state_size(),
            assembly.design_size(),
            assembly_options(s),
        )?;
        let prescribed: Vec<Vec<f64>> = (0..s.nt)
            .map(|n| {
                s.mesh
                    .tets
                    .iter()
                    .flat_map(|t| {
                        let mut row = Vec::with_capacity(20);
                        row.extend(t.iter().map(|k| (s.fixed_t[n][*k] - m.t0) / m.ts));
                        for k in t {
                            for c in 0..3 {
                                row.push(s.fixed_u[n][3 * k + c] / m.us);
                            }
                        }
                        row.extend([0.0; 4]);
                        row
                    })
                    .collect()
            })
            .collect();
        let fields = (0..s.nt).map(|n| StepField { bdot: self.bdot[n], b: self.b[n] }).collect();
        let exchange = Arc::new(ElectromagneticResidual {
            element,
            local,
            incidence: inc,
            design,
            prescribed,
            fields,
            ne: s.ne,
        });
        assembly.add_interface(Arc::clone(&exchange) as Arc<dyn HistoryInterface>)?;
        self.exchange.set((exchange, pl)).map_err(|_| err("electromagnetic loads source attached twice"))
    }

    fn exchange(&self) -> Option<Arc<dyn HistoryInterface>> {
        self.exchange.get().map(|(e, _)| Arc::clone(e) as Arc<dyn HistoryInterface>)
    }

    fn energy(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<SourceEnergy> {
        let power = if self.heating { self.quantities(n, z, x)?.0.joule_power_w } else { 0.0 };
        Ok(SourceEnergy { deposition_w: power, sensible_deposition_w: power, material_production_w: 0.0 })
    }

    fn diagnostics(&self, n: usize, z: &[f64], _old: &[f64], x: &[f64]) -> CaeResult<Map<String, Value>> {
        let (q, _) = self.quantities(n, z, x)?;
        let scalars = [
            q.ground_current_a,
            q.gauge_multiplier_a,
            q.joule_power_w,
            q.induction_power_w,
            q.terminal_power_w,
            q.free_charge_power_defect_w,
            q.power_identity_error_w,
            q.conductivity_margin_s_m,
        ];
        let arrays = q
            .charge_a
            .iter()
            .chain(&q.potential_v)
            .chain(&q.terminal_current_a)
            .chain(&q.terminal_potential_v)
            .chain(&q.force_n)
            .chain(&q.torque_n_m);
        if !scalars.iter().chain(arrays).all(|v| v.is_finite()) || q.conductivity_margin_s_m <= 0.0 {
            return Err(CaeError::convergence("nonfinite or nonpositive electromagnetic ledger"));
        }
        let tol = self.p["charge_tolerance_A"].as_f64().unwrap_or(0.0);
        let ptol = self.p["power_tolerance_W"].as_f64().unwrap_or(0.0);
        let free_error = self.free.iter().map(|k| q.charge_a[*k].abs()).fold(0.0_f64, f64::max);
        let terminal_error = q
            .terminal_current_a
            .iter()
            .zip(&self.currents[n])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0_f64, f64::max);
        let net = q.ground_current_a + q.terminal_current_a.iter().sum::<f64>();
        if free_error > tol || terminal_error > tol || q.gauge_multiplier_a.abs() > tol || net.abs() > tol {
            return Err(CaeError::convergence(format!(
                "electromagnetic charge balance has not converged; history_step={n}; free_A={}; terminal_A={}; net_A={}",
                implexity_core::py_repr::repr_float(free_error),
                implexity_core::py_repr::repr_float(terminal_error),
                implexity_core::py_repr::repr_float(net)
            )));
        }
        if q.power_identity_error_w.abs() > ptol {
            return Err(CaeError::convergence(format!(
                "electromagnetic power identity violated; history_step={n}; error_W={}",
                implexity_core::py_repr::repr_float(q.power_identity_error_w)
            )));
        }
        let mut m = self.energy(n, z, x)?.to_map();
        let ids: Vec<&str> = self.current_terminals.iter().map(|t| t.id.as_str()).collect();
        m.insert("force_N".into(), json!(q.force_n));
        m.insert("torque_about_moment_reference_N_m".into(), json!(q.torque_n_m));
        m.insert("moment_reference_m".into(), json!(self.moment_reference));
        m.insert("induction_gauge_origin_m".into(), self.p["induction_gauge_origin_m"].clone());
        m.insert("terminal_contact_occupancy".into(), Value::Object(self.contact_occupancy(x)));
        m.insert("terminal_contact_threshold".into(), json!(self.contact_threshold));
        m.insert("joule_power_W".into(), json!(q.joule_power_w));
        m.insert("induction_power_W".into(), json!(q.induction_power_w));
        m.insert("terminal_power_W".into(), json!(q.terminal_power_w));
        m.insert("free_charge_power_defect_W".into(), json!(q.free_charge_power_defect_w));
        m.insert("power_identity_error_W".into(), json!(q.power_identity_error_w));
        m.insert(
            "terminal_current_A".into(),
            Value::Object(
                ids.iter().zip(&q.terminal_current_a).map(|(k, v)| ((*k).to_string(), json!(v))).collect(),
            ),
        );
        m.insert(
            "terminal_potential_V".into(),
            Value::Object(
                ids.iter().zip(&q.terminal_potential_v).map(|(k, v)| ((*k).to_string(), json!(v))).collect(),
            ),
        );
        m.insert("ground_current_A".into(), json!(q.ground_current_a));
        m.insert("maximum_free_charge_residual_A".into(), json!(free_error));
        m.insert("maximum_terminal_current_error_A".into(), json!(terminal_error));
        m.insert("gauge_multiplier_A".into(), json!(q.gauge_multiplier_a));
        m.insert("conductivity_margin_S_m".into(), json!(q.conductivity_margin_s_m));
        m.insert("magnetic_flux_density_T".into(), json!(self.b[n]));
        m.insert("magnetic_flux_density_rate_T_s".into(), json!(self.bdot[n]));
        m.extend(self.screen(n, x));
        m.insert("joule_heating_in_thermal_rows".into(), json!(self.heating));
        m.insert("scope".into(), json!("quasistatic_uniform_field_eddy_and_injected_current"));
        m.insert("sampling".into(), json!("endpoint"));
        m.insert("data_provenance".into(), self.p["provenance"].clone());
        m.insert("experimental_qualification_verified".into(), json!(false));
        Ok(m)
    }

    fn validate(&self, history: &[Vec<f64>], x: &[f64]) -> CaeResult<()> {
        if history.len() != self.solid.times.len() {
            return Err(err("complete electromagnetic host history required"));
        }
        self.check_contacts(x)?;
        for n in 1..history.len() {
            self.diagnostics(n, &history[n], &history[n - 1], x)?;
        }
        Ok(())
    }

    fn responses(&self, history: &[Vec<f64>], x: &[f64]) -> CaeResult<Vec<f64>> {
        Ok(self.response_values(history, x, None)?.0)
    }

    fn response_vjp(&self, history: &[Vec<f64>], x: &[f64], weights: &[f64]) -> CaeResult<ResponseVjp> {
        let (_, zb, xb) = self.response_values(history, x, Some(weights))?;
        Ok((zb, xb))
    }

    fn fields(
        &self,
        history: &[Vec<f64>],
        x: &[f64],
    ) -> CaeResult<(BTreeMap<String, FieldValue>, Map<String, Value>)> {
        let s = &self.solid;
        let nc = s.nc;
        let nt = history.len();
        let volume: f64 = (0..3).map(|a| x[nc + a] * 1e-3).product::<f64>() / 6.0;
        let mut counts = vec![0.0; nc];
        for o in &s.mesh.owners {
            counts[*o] += 1.0;
        }
        let mut potential = Vec::new();
        let mut current = Vec::new();
        let mut force = Vec::new();
        let mut joule = Vec::new();
        for (n, z) in history.iter().enumerate() {
            let (q, t) = self.quantities(n, z, x)?;
            potential.extend(q.potential_v);
            let mut cj = vec![[0.0; 3]; nc];
            let mut cf = vec![[0.0; 3]; nc];
            let mut cq = vec![0.0; nc];
            for (e, term) in t.iter().enumerate() {
                let o = s.mesh.owners[e];
                for a in 0..3 {
                    cj[o][a] += term.current_density[a];
                    cf[o][a] += term.force_n[a] / volume;
                }
                cq[o] += term.joule_w / volume;
            }
            for c in 0..nc {
                current.extend(cj[c].map(|v| v / counts[c]));
                force.extend(cf[c].map(|v| v / counts[c]));
                joule.push(cq[c] / counts[c]);
            }
        }
        let mut values = BTreeMap::new();
        values.insert(
            "electromagnetic_potential_history_V".to_string(),
            FieldValue::Array(array(potential, &[nt, s.nn])?),
        );
        values.insert(
            "electromagnetic_current_density_history_A_m2".to_string(),
            FieldValue::Array(array(current, &[nt, nc, 3])?),
        );
        values.insert(
            "electromagnetic_force_density_history_N_m3".to_string(),
            FieldValue::Array(array(force, &[nt, nc, 3])?),
        );
        values.insert(
            "electromagnetic_joule_density_history_W_m3".to_string(),
            FieldValue::Array(array(joule, &[nt, nc])?),
        );
        let times: Vec<Value> = self.p["times_s"].as_array().map(|t| t[..nt].to_vec()).unwrap_or_default();
        let common = json!({"source": NAME, "times_s": times, "association": "exact_electromagnetic_history",
            "initial_state": "time zero is not solved; potentials and currents start at zero"});
        let with = |extra: Value| -> Value {
            let mut m = common.as_object().cloned().unwrap_or_default();
            if let Value::Object(e) = extra {
                m.extend(e);
            }
            Value::Object(m)
        };
        let gauge = &self.p["induction_gauge_origin_m"];
        let origin = if gauge.is_null() {
            json!("domain_centre (fixes only the potential gauge; currents are independent of it)")
        } else {
            json!({"point_m": gauge, "frame": FRAME})
        };
        let mut meta = Map::new();
        meta.insert(
            "electromagnetic_potential_history_V".into(),
            with(json!({"units": "V", "axes": ["time", "node"], "rank": "scalar", "node_order": NODE_ORDER, "induction_gauge_origin": origin})),
        );
        for (name, unit) in [
            ("electromagnetic_current_density_history_A_m2", "A/m^2"),
            ("electromagnetic_force_density_history_N_m3", "N/m^3"),
        ] {
            meta.insert(name.into(), with(json!({"units": unit, "axes": ["time", "cell", "component"], "rank": "vector",
                "components": ["x", "y", "z"], "grid": s.grid, "cell_order": "native_C_order", "cell_value": "mean_of_six_tetrahedra"})));
        }
        meta.insert("electromagnetic_joule_density_history_W_m3".into(), with(json!({"units": "W/m^3", "axes": ["time", "cell"],
            "rank": "scalar", "grid": s.grid, "cell_order": "native_C_order", "cell_value": "mean_of_six_tetrahedra"})));
        Ok((values, meta))
    }
}

type ResponseParts = (Vec<f64>, Vec<Vec<f64>>, Vec<f64>);

impl BoundElectromagneticLoads {
    #[allow(clippy::too_many_lines)]
    fn response_values(
        &self,
        history: &[Vec<f64>],
        x: &[f64],
        weights: Option<&[f64]>,
    ) -> CaeResult<ResponseParts> {
        let (exchange, _) = self.exchange_ref()?;
        let s = &self.solid;
        let nc = s.nc;
        let (r, h) = self.positions(x);
        let volume = h[0] * h[1] * h[2] / 6.0;
        let arm: Vec<[f64; 3]> =
            r.iter().map(|p| std::array::from_fn(|a| p[a] - self.moment_reference[a])).collect();
        let steps = history.len() - 1;
        let mut forces = Vec::new();
        let mut torques = Vec::new();
        let mut densities = Vec::new();
        let mut currents = Vec::new();
        let mut joule = Vec::new();
        let mut terms = Vec::new();
        for n in 1..=steps {
            let t = exchange.terms(n, &history[n], x);
            let mut f = [0.0; 3];
            let mut m = [0.0; 3];
            for (e, term) in t.iter().enumerate() {
                let c = cross(&arm[e], &term.force_n);
                for a in 0..3 {
                    f[a] += term.force_n[a];
                    m[a] += c[a];
                }
                densities.push(term.force_n.iter().map(|v| v * v).sum::<f64>() / (volume * volume));
                currents.push(term.current_density.iter().map(|v| v * v).sum::<f64>());
            }
            joule.push(t.iter().map(|v| v.joule_w).sum::<f64>());
            forces.push(f);
            torques.push(m);
            terms.push(t);
        }
        let dt = &self.dt[1..=steps];
        let fsq: Vec<f64> = forces.iter().map(|f| f.iter().map(|v| v * v).sum()).collect();
        let msq: Vec<f64> = torques.iter().map(|m| m.iter().map(|v| v * v).sum()).collect();
        let (fpeak, fgrad) = pmean(&fsq, self.order);
        let (mpeak, mgrad) = pmean(&msq, self.order);
        let (dpeak, dgrad) = pmean(&densities, self.order);
        let (cpeak, cgrad) = pmean(&currents, self.order);
        let impulse: [f64; 3] = std::array::from_fn(|a| forces.iter().zip(dt).map(|(f, d)| f[a] * d).sum());
        let angular: [f64; 3] = std::array::from_fn(|a| torques.iter().zip(dt).map(|(m, d)| m[a] * d).sum());
        let energy: f64 = joule.iter().zip(dt).map(|(j, d)| j * d).sum();
        let values = vec![
            fpeak, impulse[0], impulse[1], impulse[2], mpeak, angular[0], angular[1], angular[2], dpeak,
            cpeak, energy,
        ];
        let Some(w) = weights else { return Ok((values, Vec::new(), Vec::new())) };
        let wv = |k: usize| w.get(k).copied().unwrap_or(0.0);
        let ne = s.ne;
        let mut zb: Vec<Vec<f64>> = history.iter().map(|z| vec![0.0; z.len()]).collect();
        let mut xb = vec![0.0; x.len()];
        let mut volume_bar = 0.0;
        let mut arm_bar = vec![[0.0; 3]; ne];
        for (k, n) in (1..=steps).enumerate() {
            let f = forces[k];
            let m = torques[k];
            let f_bar: [f64; 3] = std::array::from_fn(|a| wv(0) * fgrad[k] * 2.0 * f[a] + wv(1 + a) * dt[k]);
            let m_bar: [f64; 3] = std::array::from_fn(|a| wv(4) * mgrad[k] * 2.0 * m[a] + wv(5 + a) * dt[k]);
            let joule_bar = wv(10) * dt[k];
            let t = &terms[k];
            let z = &history[n];
            for e in 0..ne {
                let term = &t[e];
                let idx = k * ne + e;

                let mxa = cross(&m_bar, &arm[e]);
                let fsq_e: f64 = term.force_n.iter().map(|v| v * v).sum();
                let fe_bar: [f64; 3] = std::array::from_fn(|a| {
                    f_bar[a] + mxa[a] + wv(8) * dgrad[idx] * 2.0 * term.force_n[a] / (volume * volume)
                });
                let arm_e = cross(&term.force_n, &m_bar);
                for a in 0..3 {
                    arm_bar[e][a] += arm_e[a];
                }
                volume_bar += wv(8) * dgrad[idx] * (-2.0 * fsq_e / (volume * volume * volume));
                let j_bar: [f64; 3] =
                    std::array::from_fn(|a| wv(9) * cgrad[idx] * 2.0 * term.current_density[a]);
                if fe_bar.iter().chain(&j_bar).all(|v| *v == 0.0) && joule_bar == 0.0 {
                    continue;
                }
                let (c, d) = exchange.local_inputs(n, z, x, e);
                let vars: Vec<Dual<25>> =
                    c.iter().chain(d.iter()).enumerate().map(|(i, v)| Dual::variable(*v, i)).collect();
                let dual = exchange.element.terms(e, &vars[..20], &vars[20..], exchange.fields[n]);
                let mut objective = dual.joule_w * Dual::from_f64(joule_bar);
                for a in 0..3 {
                    objective = objective
                        + dual.force_n[a] * Dual::from_f64(fe_bar[a])
                        + dual.current_density[a] * Dual::from_f64(j_bar[a]);
                }
                for i in 0..20 {
                    if let Ok(row) = usize::try_from(exchange.incidence[26 * e + i]) {
                        zb[n][row] += objective.eps[i];
                    }
                }
                for i in 0..5 {
                    xb[usize::try_from(exchange.design[5 * e + i]).unwrap_or(0)] += objective.eps[20 + i];
                }
            }
        }
        for a in 0..3 {
            let arm_h: f64 = (0..ne).map(|e| arm_bar[e][a] * self.centroid[e][a]).sum();
            let others: f64 = (0..3).filter(|b| *b != a).map(|b| h[b]).product();
            xb[nc + a] += (arm_h + volume_bar * others / 6.0) * 1e-3;
        }
        Ok((values, zb, xb))
    }
}

#[must_use]
pub fn editor_schema(settings: &Value, _context: &Value) -> Value {
    let Some(s) = settings.as_object() else { return json!({}) };
    let times: Vec<Value> = s.get("times_s").and_then(Value::as_array).cloned().unwrap_or_default();
    let title = |i: usize, t: &Value| match t.as_f64().filter(|v| t.is_number() && f64::is_finite(*v)) {
        Some(v) => format!("Time {} s", implexity_core::extensions::format_g6(v)),
        None => format!("Time node {i}"),
    };
    let items: Vec<Value> = times
        .iter()
        .enumerate()
        .map(|(i, t)| {
            json!({"type": "array", "minItems": 3, "maxItems": 3, "prefixItems": [
            {"title": "Bx", "units": "T", "type": "number"}, {"title": "By", "units": "T", "type": "number"},
            {"title": "Bz", "units": "T", "type": "number"}], "title": title(i, t)})
        })
        .collect();
    let nt = times.len();
    let field = json!({"title": "Uniform magnetic flux density",
        "description": "One vector per host time. Its backward difference between host times induces eddy currents; the endpoint value sets J x B.",
        "properties": {"layout": {"title": "Spatial layout", "enum": ["uniform_per_time"]},
            "values": {"title": "Flux-density history", "units": "T", "type": "array", "minItems": nt, "maxItems": nt, "prefixItems": items}}});
    let terminal = json!({"type": "object", "properties": {
        "id": {"title": "Terminal id", "type": "string"},
        "role": {"title": "Role", "enum": ["ground", "current"], "description": "ground: held at 0 V, returns the net current; current: equipotential patch with prescribed current into the domain."},
        "axis": {"title": "Face axis", "enum": [0, 1, 2]}, "side": {"title": "Face side", "enum": ["lo", "hi"]},
        "lower_fraction": {"title": "Patch lower bound (tangential fractions)", "type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "number", "minimum": 0, "maximum": 1}},
        "upper_fraction": {"title": "Patch upper bound (tangential fractions)", "type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "number", "minimum": 0, "maximum": 1}},
        "current_A": {"title": "Injected current history", "units": "A", "type": "array", "minItems": nt, "maxItems": nt, "items": {"type": "number"}, "description": "Only for current terminals; zero at the initial time."}}});
    json!({"properties": {
        "magnetic_flux_density_T": field,
        "terminals": {"title": "Current terminals", "type": "array", "items": terminal, "maxItems": 32},
        "conductivity_S_m": {"title": "Endmember conductivities at initial temperature", "units": "S/m", "type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "number", "exclusiveMinimum": 0}},
        "conductivity_slope_S_m_K": {"title": "Endmember conductivity temperature slopes", "units": "S/(m K)", "type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "number"}},
        "void_conductivity_S_m": {"title": "Complement (void/fluid) conductivity", "units": "S/m", "type": "number", "exclusiveMinimum": 0},
        "penalty": {"title": "Conductivity occupancy exponent", "type": "number", "minimum": 1},
        "joule_heating": {"title": "Add Joule heat to the shared temperature", "type": "boolean"},
        "potential_scale_V": {"title": "Potential scale", "units": "V", "type": "number", "exclusiveMinimum": 0},
        "current_residual_scale_A": {"title": "Charge residual scale", "units": "A", "type": "number", "exclusiveMinimum": 0},
        "charge_tolerance_A": {"title": "Charge balance tolerance", "units": "A", "type": "number", "exclusiveMinimum": 0},
        "power_tolerance_W": {"title": "Power identity tolerance", "units": "W", "type": "number", "exclusiveMinimum": 0},
        "response_norm_order": {"title": "Peak-response norm order", "type": "integer", "minimum": 2, "maximum": 64, "multipleOf": 2, "description": format!("Order p of every peak response, a {PNORM}.")},
        "induction_gauge_origin_m": {"title": "Induction gauge origin", "units": "m", "type": ["array", "null"], "minItems": 3, "maxItems": 3, "items": {"type": "number"},
            "description": format!("Point to which the external circuit leads run straight from every terminal (its return path links no further flux). Required when terminals and a changing field coexist; null otherwise, which only fixes the potential gauge at the domain centre. {FRAME}.")},
        "moment_reference_m": {"title": "Torque reference point", "units": "m", "type": "array", "minItems": 3, "maxItems": 3, "items": {"type": "number"},
            "description": format!("Point about which torque responses are taken. Without terminals the net force vanishes and the torque does not depend on it. {FRAME}.")},
        "provenance": {"title": "Field, current and conductivity data provenance", "type": "string"}}})
}

#[must_use]
pub fn study_templates(context: &Value) -> Vec<Value> {
    let Some(ctx) = context.as_object() else { return Vec::new() };
    let solid = ctx.get("solid").cloned().unwrap_or(Value::Null);
    let existing = match ctx.get("field_sources") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return Vec::new(),
    };
    if !solid.is_object()
        || existing.iter().any(|r| !r.is_object() || r.get("component") == Some(&json!(NAME)))
    {
        return Vec::new();
    }
    let (grid, times) = (solid["grid"].clone(), solid["times_s"].clone());
    if grid_of(&grid).is_none() || times.as_array().is_none_or(|t| t.len() < 2) {
        return Vec::new();
    }
    let names: Vec<Value> =
        existing.iter().map(|r| r.get("component").cloned().unwrap_or(Value::Null)).collect();
    if names.contains(&json!(EXCLUSIVE.0)) {
        return Vec::new();
    }
    let nt = times.as_array().map_or(0, Vec::len);
    let joule = !names.iter().any(|n| JOULE_SOURCES.iter().any(|j| n == &json!(j)));
    let base = json!({"grid": grid, "times_s": times, "node_order": NODE_ORDER,
        "magnetic_flux_density_T": {"layout": "uniform_per_time", "values": vec![[0.0, 0.0, 0.0]; nt]},
        "induction_convention": INDUCTION, "induction_gauge_origin_m": null, "terminals": [], "conductivity_S_m": [1.0, 1.0],
        "conductivity_slope_S_m_K": [0.0, 0.0], "void_conductivity_S_m": 1e-6, "penalty": 3.0, "joule_heating": joule,
        "potential_scale_V": 1.0, "current_residual_scale_A": 1.0, "charge_tolerance_A": 1e-8, "power_tolerance_W": 1e-8,
        "response_norm_order": 8, "moment_reference_m": [0.0, 0.0, 0.0], "energy_convention": ENERGY,
        "provenance": "Zero-load authoring starter; replace conductivities, field and currents with sourced data"});
    let d = data();
    let texts = d["study_template_text"].as_array().cloned().unwrap_or_default();
    let mut variants = vec![json!([])];
    for axis in 0..3 {
        variants.push(json!([
            {"id": "ground", "role": "ground", "axis": axis, "side": "lo", "lower_fraction": [0.0, 0.0], "upper_fraction": [1.0, 1.0]},
            {"id": "terminal", "role": "current", "axis": axis, "side": "hi", "lower_fraction": [0.0, 0.0], "upper_fraction": [1.0, 1.0], "current_A": vec![0.0; nt]}]));
    }
    let mut out = Vec::new();
    for (k, terminals) in variants.into_iter().enumerate() {
        let mut settings = base.clone();
        settings["terminals"] = terminals;
        let Ok(settings) = normalise(&settings, context) else { return Vec::new() };
        let t = &texts[k];
        let mut prefix: Vec<Value> = existing.iter().map(|_| json!({})).collect();
        prefix.push(json!({"title": d["editor_label"], "properties": {"component": {"title": "Source component", "enum": [NAME]},
            "settings": editor_schema(&settings, context)}}));
        let mut patch = existing.clone();
        patch.push(json!({"component": NAME, "settings": settings}));
        out.push(json!({"schema": "implexity-provider-study-template/1", "id": t["id"], "label": t["label"],
            "description": t["description"], "truth_status": t["truth_status"],
            "problem_requirements": [{"path": ["solid", "grid"], "value": grid}, {"path": ["solid", "times_s"], "value": times},
                {"path": ["field_sources"], "value": ctx.get("field_sources").cloned().unwrap_or(Value::Null), "missing_equals_null": true}],
            "problem_patch": {"field_sources": patch},
            "editor_schema_patch": {"properties": {"field_sources": {"title": "Coupled field sources", "type": "array", "prefixItems": prefix}}}}));
    }
    out
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ElectromagneticLoads;

impl HistorySourceComponent for ElectromagneticLoads {
    fn validate(&self, settings: &Value, context: &dyn Any) -> CaeResult<Value> {
        let context = context
            .downcast_ref::<Value>()
            .ok_or_else(|| err("electromagnetic loads require a native solid context"))?;
        normalise(settings, context)
    }

    fn create(&self, settings: &Value, host: &dyn Any) -> CaeResult<Box<dyn Any + Send + Sync>> {
        let host = host_of(host)?;
        let p = normalise(settings, host.problem())?;
        Ok(Box::new(BoundSource(Arc::new(BoundElectromagneticLoads::new(p, host.as_ref())?))))
    }

    fn coupling(&self, base: Value, settings: &Value) -> CaeResult<Value> {
        let node = BLOCK;
        let mut edges = vec![
            (
                node,
                "structure",
                "lorentz_body_force",
                "monolithic",
                "J x B from the solved potential enters the solid momentum rows of the same Newton system",
            ),
            (
                "thermal",
                node,
                "temperature_dependent_conductivity",
                "monolithic",
                "element-mean temperature sets the endmember conductivities",
            ),
        ];
        if settings["joule_heating"] == json!(true) {
            edges.push((
                node,
                "thermal",
                "resolved_joule_heat",
                "monolithic",
                "J.E enters the shared temperature rows",
            ));
        }
        let with_node = extend_coupling(&base, &[node], &edges, &[], &limitations())?;

        let mut d = implexity_core::coupling_graph::CouplingDeclaration::from_value(&with_node)
            .map_err(|e| err(e.0))?;
        d.closed_loops.push(d.active_physics.clone());
        Ok(d.to_value())
    }

    fn owns_material_forcing(&self, _settings: &Value) -> bool {
        false
    }
}

impl FieldSourceAuthoring for ElectromagneticLoads {
    fn editor_schema(&self, settings: &Value, context: &Value) -> Value {
        editor_schema(settings, context)
    }

    fn study_templates(&self, context: &Value) -> Vec<Value> {
        study_templates(context)
    }
}
