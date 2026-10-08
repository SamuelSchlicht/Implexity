// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use implexity_ad::Scalar;
use implexity_core::CaeError;
use implexity_linalg::sparse::CsrMatrix;
use implexity_physics_base::thermal_exchange_law::{
    EXCHANGE_INTERFACE, ExchangeInterface, ThermalExchangeLaw, exchange_flux,
};
use implexity_solve::local_assembly::{
    AssemblyOptions, Incidence, Kind, LocalResidual, LocalResidualAssembly,
};

use crate::util::{contract, convergence, f, has_exact_keys, real_array, text};


pub fn real_value(value: &Value, label: &str) -> Result<f64, CaeError> {
    match crate::util::num(value) {
        Some(v) => Ok(v),
        None => contract(format!("{label} requires a finite real number")),
    }
}


pub fn real_history(value: &Value, label: &str) -> Result<(Vec<usize>, Vec<f64>), CaeError> {
    match real_array(value) {
        Some(x) => Ok(x),
        None => {

            contract(format!("{label} requires a finite real number"))
        }
    }
}


pub fn component(name: &str) -> Result<Arc<dyn ThermalExchangeLaw>, CaeError> {
    let row = implexity_core::registries::global().addins.get(name)?;
    let law = row
        .adapter
        .as_ref()
        .filter(|a| a.component_kind().as_deref() == Some("thermal_exchange"))
        .and_then(|a| a.interface(EXCHANGE_INTERFACE))
        .and_then(|i| i.downcast_ref::<ExchangeInterface>())
        .map(|i| Arc::clone(&i.0));
    law.ok_or_else(|| {
        CaeError::contract(format!(
            "{} is not an active thermal-exchange component",
            implexity_core::py_repr::repr_str(name)
        ))
    })
}


pub fn validate_exchange(p: &mut Value) -> Result<(), CaeError> {
    let nt = crate::util::time_count(p);
    let reservoirs = p.get("thermal_reservoirs").cloned().unwrap_or_else(|| json!([]));
    let exchanges = p.get("thermal_exchanges").cloned().unwrap_or_else(|| json!([]));
    let (Some(rows), Some(links)) = (reservoirs.as_array(), exchanges.as_array()) else {
        return contract("thermal_reservoirs and thermal_exchanges must be arrays");
    };
    let mut by: Vec<String> = Vec::new();
    for row in rows {
        if !row.is_object() {
            return contract("thermal reservoir must be an object");
        }
        let kind = row.get("kind").cloned().unwrap_or(Value::Null);
        let mut keys = vec!["id", "kind", "provenance", "T_min_K", "T_max_K"];
        if kind == json!("finite_capacity") {
            keys.extend(["capacity_J_K", "initial_temperature_K", "source_W"]);
        } else if kind == json!("prescribed") {
            keys.push("temperatures_K");
        } else {
            return contract("reservoir kind must be finite_capacity or prescribed");
        }
        if !has_exact_keys(row, &keys) {
            return contract(format!(
                "{} reservoir requires keys {}",
                kind.as_str().unwrap_or_default(),
                crate::util::sorted_repr(keys)
            ));
        }
        let id = row["id"].as_str().filter(|s| !s.trim().is_empty() && !by.iter().any(|b| b == s));
        let Some(id) = id.filter(|_| row["id"].is_string()) else {
            return contract("thermal reservoir identities must be nonempty and unique");
        };
        if !text(&row["provenance"]) {
            return contract("thermal reservoir provenance required");
        }
        let tmin = real_value(&row["T_min_K"], "minimum temperature")?;
        let tmax = real_value(&row["T_max_K"], "maximum temperature")?;
        if !(0.0 < tmin && tmin < tmax) {
            return contract("invalid thermal reservoir temperature range");
        }
        if kind == json!("finite_capacity") {
            let c = real_value(&row["capacity_J_K"], "reservoir capacity")?;
            let t = real_value(&row["initial_temperature_K"], "initial temperature")?;
            let (shape, q) = real_history(&row["source_W"], "reservoir source")?;
            if c <= 0.0 || !(tmin <= t && t <= tmax) {
                return contract("finite reservoir requires positive capacity and valid initial temperature");
            }
            if shape != [nt] || q[0] != 0.0 {
                return contract("reservoir heat-source history must be finite and start at zero");
            }
        } else {
            let (shape, t) = real_history(&row["temperatures_K"], "prescribed temperature")?;
            if shape != [nt] || t.iter().any(|v| *v < tmin || *v > tmax) {
                return contract("invalid prescribed reservoir temperature history");
            }
        }
        by.push(id.to_string());
    }
    let mut ids: Vec<String> = Vec::new();
    for row in links {
        if !has_exact_keys(row, &["axis", "component", "id", "parameters", "reservoir", "side"]) {
            return contract("thermal exchange requires id, axis, side, component, parameters and reservoir");
        }
        let id = row["id"].as_str().filter(|s| !s.trim().is_empty() && !ids.iter().any(|b| b == s));
        let Some(id) = id else {
            return contract("thermal-exchange identities must be nonempty and unique");
        };
        ids.push(id.to_string());
        let axis_ok = row["axis"].as_i64().is_some_and(|a| (0..=2).contains(&a))
            && (row["axis"].is_i64() || row["axis"].is_u64());
        let side_ok = row["side"] == json!("lo") || row["side"] == json!("hi");
        if !axis_ok || !side_ok {
            return contract("thermal exchange requires an oriented boundary face");
        }
        if !row["reservoir"].as_str().is_some_and(|r| by.iter().any(|b| b == r)) {
            return contract("thermal exchange references an unknown reservoir");
        }
        let Some(name) = row["component"].as_str() else {
            return contract("thermal exchange component must be a registered name");
        };
        component(name)?.validate(&row["parameters"])?;
    }
    p["thermal_reservoirs"] = reservoirs;
    p["thermal_exchanges"] = exchanges;
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reservoir {
    pub id: String,
    pub finite: bool,
    pub t_min: f64,
    pub t_max: f64,
    pub capacity: f64,
    pub initial: f64,
    pub source: Vec<f64>,
    pub temperatures: Vec<f64>,
}

impl Reservoir {
    fn from_json(r: &Value) -> Self {
        let list = |k: &str| real_array(&r[k]).map(|(_, v)| v).unwrap_or_default();
        Self {
            id: r["id"].as_str().unwrap_or_default().to_string(),
            finite: r["kind"] == json!("finite_capacity"),
            t_min: f(r, "T_min_K"),
            t_max: f(r, "T_max_K"),
            capacity: f(r, "capacity_J_K"),
            initial: f(r, "initial_temperature_K"),
            source: list("source_W"),
            temperatures: list("temperatures_K"),
        }
    }
}

#[derive(Clone)]
pub struct ExchangeElement {
    law: Arc<dyn ThermalExchangeLaw>,
    parameters: Value,
    weights: Vec<f64>,
    axes: [usize; 2],
    t0: f64,
    ts: f64,
    scale: f64,
}

impl LocalResidual for ExchangeElement {
    fn residual<S: Scalar>(&self, item: usize, current: &[S], _previous: &[S], spacing: &[S], out: &mut [S]) {
        let a = current[0] * self.ts + self.t0;
        let b = current[1] * self.ts + self.t0;
        let area = spacing[self.axes[0]] * spacing[self.axes[1]] * (self.weights[item] * 1e-6);
        let q = area * exchange_flux(self.law.as_ref(), a, b, &self.parameters);
        out[0] = q / self.scale;
        out[1] = -q / self.scale;
    }
}

#[derive(Debug, Clone)]
pub struct CapacityElement {
    capacity: Vec<f64>,
    ts: f64,
    scale: f64,
}

impl LocalResidual for CapacityElement {
    fn residual<S: Scalar>(&self, item: usize, current: &[S], previous: &[S], _design: &[S], out: &mut [S]) {
        let dt = current[1];
        let source = current[2];
        out[0] = ((current[0] - previous[0]) * (self.capacity[item] * self.ts) / dt - source) / self.scale;
    }
}

pub struct ExchangeGroup {
    pub entry: Value,
    pub face: Vec<usize>,
    pub reservoir: usize,
    pub weights: Vec<f64>,
    pub assembly: LocalResidualAssembly<ExchangeElement>,
    columns: Vec<i64>,
    law: Arc<dyn ThermalExchangeLaw>,
    parameters: Value,
}

#[derive(Debug, Clone)]
pub struct BoundaryHost {
    pub t0: f64,
    pub ts: f64,
    pub scale: f64,
    pub state_size: usize,
    pub solid_state_size: usize,
    pub nc: usize,
    pub fixed_t: Vec<Vec<f64>>,
    pub times: Vec<f64>,
    pub batch_size: usize,
}

pub struct SolidBoundaryHistory {
    pub host: BoundaryHost,
    pub reservoirs: Vec<Reservoir>,
    pub indices: BTreeMap<String, usize>,
    pub groups: Vec<ExchangeGroup>,
    pub capacity: Option<LocalResidualAssembly<CapacityElement>>,
    finite: Vec<usize>,
}

impl SolidBoundaryHistory {

    pub fn new(
        host: BoundaryHost,
        problem: &Value,
        tmap: &[i64],
        face_weights: &dyn Fn(usize, bool) -> (Vec<usize>, Vec<f64>),
    ) -> Result<Self, CaeError> {
        let reservoirs: Vec<Reservoir> = problem["thermal_reservoirs"]
            .as_array()
            .into_iter()
            .flatten()
            .map(Reservoir::from_json)
            .collect();
        let finite: Vec<usize> = (0..reservoirs.len()).filter(|i| reservoirs[*i].finite).collect();
        let indices: BTreeMap<String, usize> = finite
            .iter()
            .enumerate()
            .map(|(k, i)| (reservoirs[*i].id.clone(), host.solid_state_size + k))
            .collect();
        let design = |count: usize| -> Result<Incidence, CaeError> {
            let nc = i64::try_from(host.nc).unwrap_or(0);
            Incidence::new(count, 3, (0..count).flat_map(|_| [nc, nc + 1, nc + 2]).collect())
        };
        let options = || AssemblyOptions { batch_size: host.batch_size, ..AssemblyOptions::default() };
        let mut groups = Vec::new();
        for entry in problem["thermal_exchanges"].as_array().into_iter().flatten() {
            let axis = usize::try_from(entry["axis"].as_u64().unwrap_or(0)).unwrap_or(0);
            let hi = entry["side"] == json!("hi");
            let (face, weights) = face_weights(axis, hi);
            let reservoir = reservoirs.iter().position(|r| entry["reservoir"] == json!(r.id)).unwrap_or(0);
            let ri = indices.get(&reservoirs[reservoir].id).map_or(-1, |i| i64::try_from(*i).unwrap_or(-1));
            let rows: Vec<i64> = face.iter().flat_map(|n| [tmap[*n], ri]).collect();
            let columns = rows.clone();
            let rows = Incidence::new(face.len(), 2, rows)?;
            let axes: Vec<usize> = (0..3).filter(|i| *i != axis).collect();
            let law = component(entry["component"].as_str().unwrap_or_default())?;
            let parameters = law.validate(&entry["parameters"])?;
            let kernel = ExchangeElement {
                law: Arc::clone(&law),
                parameters: parameters.clone(),
                weights: weights.clone(),
                axes: [axes[0], axes[1]],
                t0: host.t0,
                ts: host.ts,
                scale: host.scale,
            };
            let assembly = LocalResidualAssembly::new(
                kernel,
                rows.clone(),
                rows.clone(),
                rows,
                design(face.len())?,
                host.state_size,
                2 * host.nc + 3,
                options(),
            )?;
            groups.push(ExchangeGroup {
                entry: entry.clone(),
                face,
                reservoir,
                weights,
                assembly,
                columns,
                law,
                parameters,
            });
        }
        let capacity = if finite.is_empty() {
            None
        } else {
            let idx: Vec<i64> =
                finite.iter().map(|i| i64::try_from(indices[&reservoirs[*i].id]).unwrap_or(-1)).collect();
            let rows = Incidence::new(finite.len(), 1, idx.clone())?;
            let cols = Incidence::new(finite.len(), 3, idx.iter().flat_map(|i| [*i, -1, -1]).collect())?;
            let kernel = CapacityElement {
                capacity: finite.iter().map(|i| reservoirs[*i].capacity).collect(),
                ts: host.ts,
                scale: host.scale,
            };
            Some(LocalResidualAssembly::new(
                kernel,
                rows,
                cols.clone(),
                cols,
                design(finite.len())?,
                host.state_size,
                2 * host.nc + 3,
                options(),
            )?)
        };
        Ok(Self { host, reservoirs, indices, groups, capacity, finite })
    }

    #[must_use]
    pub fn has_state_terms(&self) -> bool {
        !self.groups.is_empty() || self.capacity.is_some()
    }

    #[must_use]
    pub fn data(&self, group: &ExchangeGroup, n: usize) -> (Vec<f64>, Vec<f64>) {
        let h = &self.host;
        let r = &self.reservoirs[group.reservoir];
        let mut a = vec![0.0; 2 * group.face.len()];
        let mut b = a.clone();
        for (k, node) in group.face.iter().enumerate() {
            a[2 * k] = (h.fixed_t[n][*node] - h.t0) / h.ts;
            b[2 * k] = (h.fixed_t[n - 1][*node] - h.t0) / h.ts;
            if !r.finite {
                a[2 * k + 1] = (r.temperatures[n] - h.t0) / h.ts;
                b[2 * k + 1] = (r.temperatures[n - 1] - h.t0) / h.ts;
            }
        }
        (a, b)
    }

    #[must_use]
    pub fn group_node_values(&self, group: &ExchangeGroup, n: usize, z: &[f64], x: &[f64]) -> Vec<f64> {
        let (data, _) = self.data(group, n);
        let nc = self.host.nc;
        let spacing = [x[nc], x[nc + 1], x[nc + 2]];
        let kernel = group.assembly.kernel();
        (0..group.face.len())
            .map(|k| {
                let local: Vec<f64> = (0..2)
                    .map(|j| usize::try_from(group.columns[2 * k + j]).map_or(data[2 * k + j], |i| z[i]))
                    .collect();
                let mut out = [0.0; 2];
                kernel.residual(k, &local, &local, &spacing, &mut out);
                out[0]
            })
            .collect()
    }

    #[must_use]
    pub fn capacity_data(&self, n: usize) -> (Vec<f64>, Vec<f64>) {
        let dt = self.host.times[n] - self.host.times[n - 1];
        let current: Vec<f64> =
            self.finite.iter().flat_map(|i| [0.0, dt, self.reservoirs[*i].source[n]]).collect();
        (current.clone(), current)
    }

    #[must_use]
    pub fn initial_state(&self, mut z: Vec<f64>) -> Vec<f64> {
        for i in &self.finite {
            let r = &self.reservoirs[*i];
            z[self.indices[&r.id]] = (r.initial - self.host.t0) / self.host.ts;
        }
        z
    }


    pub fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> Result<Vec<f64>, CaeError> {
        let mut result = vec![0.0; self.host.state_size];
        for g in &self.groups {
            let (a, b) = self.data(g, n);
            for (r, v) in result.iter_mut().zip(g.assembly.residual(z, old, x, &a, &b)?) {
                *r += v;
            }
        }
        if let Some(c) = &self.capacity {
            let (a, b) = self.capacity_data(n);
            for (r, v) in result.iter_mut().zip(c.residual(z, old, x, &a, &b)?) {
                *r += v;
            }
        }
        Ok(result)
    }


    pub fn jacobians(
        &self,
        kind: Kind,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
    ) -> Result<Vec<CsrMatrix>, CaeError> {
        let mut out = Vec::new();
        for g in &self.groups {
            let (a, b) = self.data(g, n);
            out.push(g.assembly.jacobian(kind, z, old, x, &a, &b)?);
        }
        if let Some(c) = &self.capacity {
            let (a, b) = self.capacity_data(n);
            out.push(c.jacobian(kind, z, old, x, &a, &b)?);
        }
        Ok(out)
    }


    pub fn current_action(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        vector: &[f64],
        transpose: bool,
    ) -> Result<Vec<f64>, CaeError> {
        let mut result = vec![0.0; self.host.state_size];
        for g in &self.groups {
            let (a, b) = self.data(g, n);
            for (r, v) in
                result.iter_mut().zip(g.assembly.current_action(z, old, x, &a, &b, vector, transpose)?)
            {
                *r += v;
            }
        }
        if let Some(c) = &self.capacity {
            let (a, b) = self.capacity_data(n);
            for (r, v) in result.iter_mut().zip(c.current_action(z, old, x, &a, &b, vector, transpose)?) {
                *r += v;
            }
        }
        Ok(result)
    }

    #[must_use]
    pub fn temperatures(&self, n: usize, z: &[f64]) -> Vec<(String, f64)> {
        self.reservoirs
            .iter()
            .map(|r| {
                let t = if r.finite {
                    self.host.t0 + self.host.ts * z[self.indices[&r.id]]
                } else {
                    r.temperatures[n]
                };
                (r.id.clone(), t)
            })
            .collect()
    }


    pub fn check(&self, n: usize, z: &[f64]) -> Result<(), CaeError> {
        for ((name, t), r) in self.temperatures(n, z).iter().zip(&self.reservoirs) {
            if !t.is_finite() || !(r.t_min <= *t && *t <= r.t_max) {
                return convergence(format!("thermal reservoir {name} leaves authored validity interval"));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn powers(&self, n: usize, z: &[f64], x: &[f64], nodal_temperature: &[f64]) -> Value {
        let tr = self.temperatures(n, z);
        let h = [x[self.host.nc] * 1e-3, x[self.host.nc + 1] * 1e-3, x[self.host.nc + 2] * 1e-3];
        let mut out = serde_json::Map::new();
        for g in &self.groups {
            let axis = usize::try_from(g.entry["axis"].as_u64().unwrap_or(0)).unwrap_or(0);
            let axes: Vec<usize> = (0..3).filter(|i| *i != axis).collect();
            let reservoir = &self.reservoirs[g.reservoir];
            let t_res = tr[g.reservoir].1;
            let sum: f64 = g
                .face
                .iter()
                .zip(&g.weights)
                .map(|(node, w)| {
                    w * exchange_flux(g.law.as_ref(), nodal_temperature[*node], t_res, &g.parameters)
                })
                .sum();
            let power = sum * h[axes[0]] * h[axes[1]];
            out.insert(
                g.entry["id"].as_str().unwrap_or_default().to_string(),
                json!({"solid_outward_W": power, "reservoir_inward_W": power,
                    "reservoir": reservoir.id, "component": g.entry["component"],
                    "reservoir_kind": if reservoir.finite { "finite_capacity" } else { "prescribed" }}),
            );
        }
        Value::Object(out)
    }

    #[must_use]
    pub fn ledger(&self, n: usize, z: &[f64], old: &[f64], x: &[f64], nodal_temperature: &[f64]) -> Value {
        let powers = self.powers(n, z, x, nodal_temperature);
        let current = self.temperatures(n, z);
        let previous = self.temperatures(n - 1, old);
        let dt = self.host.times[n] - self.host.times[n - 1];
        let mut reservoirs = serde_json::Map::new();
        let rows: Vec<&Value> = powers.as_object().map(|m| m.values().collect()).unwrap_or_default();
        for i in &self.finite {
            let r = &self.reservoirs[*i];
            let inward: f64 = rows
                .iter()
                .filter(|row| row["reservoir"] == json!(r.id))
                .map(|row| row["reservoir_inward_W"].as_f64().unwrap_or(0.0))
                .sum();
            let storage = r.capacity * (current[*i].1 - previous[*i].1) / dt;
            let source = r.source[n];
            reservoirs.insert(
                r.id.clone(),
                json!({"storage_W": storage, "source_W": source, "exchange_inward_W": inward, "balance_W": storage - source - inward}),
            );
        }
        let outward: f64 = rows.iter().map(|r| r["solid_outward_W"].as_f64().unwrap_or(0.0)).sum();
        let prescribed: f64 = rows
            .iter()
            .filter(|r| r["reservoir_kind"] == json!("prescribed"))
            .map(|r| r["reservoir_inward_W"].as_f64().unwrap_or(0.0))
            .sum();
        json!({"exchanges": powers, "finite_reservoirs": reservoirs,
            "solid_exchange_outward_W": outward, "prescribed_reservoir_inward_W": prescribed})
    }
}
