// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use serde_json::{Map, Value, json};

use super::catalog::{self, WALL, WORK};
use super::owner::Owner;
use super::responses::{Table, vmax, vmin};
use crate::rv::{Recording, Rv};

const TEMPERATURE: &str = "history_max_temperature_K";

fn convergence(msg: &str) -> CaeError {
    CaeError::convergence(msg)
}

fn times(owner: &Owner, states: usize) -> CaeResult<Vec<f64>> {
    let t = owner.s.times.clone();
    if t.len() < 2
        || states != t.len()
        || t.iter().any(|v| !v.is_finite())
        || t.windows(2).any(|w| w[1] - w[0] <= 0.0)
    {
        return Err(CaeError::contract("history reduction requires matching ordered physical times"));
    }
    Ok(t)
}

fn sources(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        if let Some(h) = catalog::history_definition(n)
            && h.source != "native_nodal_temperature"
            && !out.contains(&h.source)
        {
            out.push(h.source);
        }
    }
    out
}

impl Owner {
    #[must_use]
    pub fn interval_features<S: Scalar>(
        &self,
        n: usize,
        current: &[S],
        previous: &[S],
        x: &[S],
        names: &[String],
    ) -> Vec<S> {
        let ledger = self.transport_interval(n, current, previous, x);
        let iq = self.interval_quantities(n, current, previous, &ledger);
        let mut quantities: Table<S> = iq.values;
        let work: Vec<&str> = WORK.iter().chain(&WALL).map(|d| d.1).collect();
        if names.iter().any(|n| work.contains(&n.as_str())) {
            quantities.extend(self.mechanical_quantities(n, current, previous, &ledger).0);
        }
        let energy = catalog::energy_sources();
        if names.iter().any(|n| energy.contains(n)) {
            quantities.extend(self.energy_quantities(previous, current, &ledger).0);
        }
        if let Some(v) = iq.balance.get("fluid_caloric_residual_sum_W") {
            quantities.set("interval_fluid_caloric_demand_W", v);
        }
        names.iter().map(|name| quantities.get(name).unwrap_or_else(S::zero)).collect()
    }
}

fn reduce<S: Scalar>(table: &[Vec<S>], srcs: &[String], names: &[String], dt: &[f64]) -> CaeResult<Vec<S>> {
    let mut out = Vec::with_capacity(names.len());
    let span: f64 = dt.iter().sum();
    for name in names {
        let h = catalog::history_definition(name)
            .ok_or_else(|| CaeError::contract("unsupported interval reduction"))?;
        let col = srcs.iter().position(|s| *s == h.source).unwrap_or(0);
        let a: Vec<S> = table.iter().map(|row| row[col]).collect();
        let weighted = || a.iter().zip(dt).fold(S::zero(), |acc, (v, d)| acc + *v * *d);
        out.push(match h.reduction {
            "integral" => weighted(),
            "time_mean" => weighted() / span,
            "minimum" => vmin(&a),
            "maximum_absolute" => vmax(&a.iter().map(|v| v.abs()).collect::<Vec<S>>()),
            _ => return Err(CaeError::contract("unsupported interval reduction")),
        });
    }
    Ok(out)
}


pub fn observe(
    owner: &Owner,
    states: &[Vec<f64>],
    x: &[f64],
    names: &[String],
) -> CaeResult<(Map<String, Value>, Value)> {
    let t = times(owner, states.len())?;
    let dt: Vec<f64> = t.windows(2).map(|w| w[1] - w[0]).collect();
    let mut scalars = Map::new();
    let interval: Vec<String> = names.iter().filter(|n| *n != TEMPERATURE).cloned().collect();
    let srcs = sources(&interval);
    if !interval.is_empty() {
        let table: Vec<Vec<f64>> =
            (1..t.len()).map(|n| owner.interval_features(n, &states[n], &states[n - 1], x, &srcs)).collect();
        if table.iter().flatten().any(|v| !v.is_finite()) {
            return Err(convergence("nonfinite history response samples"));
        }
        for (name, v) in interval.iter().zip(reduce(&table, &srcs, &interval, &dt)?) {
            scalars.insert(name.clone(), json!(v));
        }
    }
    if names.iter().any(|n| n == TEMPERATURE) {
        let mut maxima = Vec::new();
        for (n, state) in states.iter().enumerate() {
            let (temperature, _) = owner.nodal_fields(n, state);
            if temperature.is_empty() {
                return Err(CaeError::contract("empty stored nodal temperature"));
            }
            if temperature.iter().any(|v| !v.is_finite()) {
                return Err(convergence("nonfinite stored nodal temperature"));
            }
            maxima.push(temperature.iter().copied().fold(f64::NEG_INFINITY, f64::max));
        }
        scalars.insert(TEMPERATURE.into(), json!(maxima.iter().copied().fold(f64::NEG_INFINITY, f64::max)));
    }
    if scalars.values().any(|v| !v.as_f64().is_some_and(f64::is_finite)) {
        return Err(convergence("nonfinite reduced history response"));
    }
    let ordered: Map<String, Value> = names.iter().map(|n| (n.clone(), scalars[n].clone())).collect();
    let report = json!({"schema": "implexity-porous-history-reduction/1", "stored_states": t.len(),
        "intervals": t.len() - 1, "start_time_s": t[0], "end_time_s": t[t.len() - 1],
        "duration_s": t[t.len() - 1] - t[0], "responses": names,
        "selection_changes_physics": false, "final_acceptance_performed": false,
        "continuous_time_extrema_certified": false, "total_energy_certified": false,
        "reduction_scope": "actual stored physical history, not optimizer epochs"});
    Ok((ordered, report))
}


pub fn linearize(
    owner: &Owner,
    states: &[Vec<f64>],
    x: &[f64],
    names: &[String],
) -> CaeResult<(Vec<f64>, Vec<DenseMatrix>, DenseMatrix)> {
    let t = times(owner, states.len())?;
    let dt: Vec<f64> = t.windows(2).map(|w| w[1] - w[0]).collect();
    let m = names.len();
    let nz = owner.state_size;
    let nd = owner.design_size;
    let mut gu: Vec<DenseMatrix> = (0..t.len()).map(|_| DenseMatrix::zeros(nz, m)).collect();
    let mut gx = DenseMatrix::zeros(nd, m);
    let mut output = vec![0.0; m];
    let interval: Vec<String> = names.iter().filter(|n| *n != TEMPERATURE).cloned().collect();
    let srcs = sources(&interval);
    if !interval.is_empty() {
        let table: Vec<Vec<f64>> =
            (1..t.len()).map(|n| owner.interval_features(n, &states[n], &states[n - 1], x, &srcs)).collect();
        let rec = Recording::start();
        let vars: Vec<Vec<Rv>> = table.iter().map(|row| rec.inputs(row)).collect();
        let reduced = reduce(&vars, &srcs, &interval, &dt)?;
        let flat: Vec<Rv> = vars.iter().flatten().copied().collect();
        let weights: Vec<Vec<f64>> = reduced.iter().map(|r| rec.gradient(*r, &flat)).collect();
        drop(rec);
        let indices: Vec<usize> =
            interval.iter().map(|n| names.iter().position(|q| q == n).unwrap_or(0)).collect();
        for (k, r) in reduced.iter().enumerate() {
            output[indices[k]] = r.value();
        }
        let ns = srcs.len();
        for n in 1..t.len() {
            let rec = Recording::start();
            let zc = rec.inputs(&states[n]);
            let zp = rec.inputs(&states[n - 1]);
            let xv = rec.inputs(x);
            let features = owner.interval_features(n, &zc, &zp, &xv, &srcs);
            for (k, target) in indices.iter().enumerate() {
                let w: Vec<f64> = (0..ns).map(|s| weights[k][(n - 1) * ns + s]).collect();
                if w.iter().all(|v| *v == 0.0) {
                    continue;
                }
                let all: Vec<Rv> = zc.iter().chain(&zp).chain(&xv).copied().collect();
                let g = rec.vjp(&features, &w, &all);
                let (dz, rest) = g.split_at(nz);
                let (dp, dx) = rest.split_at(nz);
                for i in 0..nz {
                    gu[n].data[i * m + target] += dz[i];
                    gu[n - 1].data[i * m + target] += dp[i];
                }
                for i in 0..nd {
                    gx.data[i * m + target] += dx[i];
                }
            }
        }
    }
    if let Some(index) = names.iter().position(|n| n == TEMPERATURE) {
        let mut maxima = Vec::new();
        let mut counts = Vec::new();
        for (n, state) in states.iter().enumerate() {
            let (temperature, _) = owner.nodal_fields(n, state);
            let mx = temperature.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            counts.push(temperature.iter().filter(|v| **v == mx).count());
            maxima.push(mx);
        }
        let maximum = maxima.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let count: usize = maxima.iter().zip(&counts).filter(|(v, _)| **v == maximum).map(|(_, c)| c).sum();
        output[index] = maximum;
        for (n, (mx, c)) in maxima.iter().zip(&counts).enumerate() {
            if *mx != maximum {
                continue;
            }
            let rec = Recording::start();
            let z = rec.inputs(&states[n]);
            let (temperature, _) = owner.nodal_fields(n, &z);
            let g = rec.gradient(vmax(&temperature), &z);
            #[allow(clippy::cast_precision_loss)]
            let share = *c as f64 / count as f64;
            for i in 0..nz {
                gu[n].data[i * m + index] += g[i] * share;
            }
        }
    }
    if output
        .iter()
        .chain(gx.data.iter())
        .chain(gu.iter().flat_map(|g| g.data.iter()))
        .any(|v| !v.is_finite())
    {
        return Err(convergence("nonfinite history response or partial"));
    }
    Ok((output, gu, gx))
}

#[must_use]
pub fn selected(p: &Value) -> Vec<String> {
    p.get("history_responses")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}
