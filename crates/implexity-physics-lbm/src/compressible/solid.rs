// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use serde_json::Value;

use crate::d3q19::Grid;
use crate::nparray::asarray;
use crate::radiation::{Surface, exchange};

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConductionMap {
    pub links_dt: Vec<[f64; 3]>,
    pub substeps: usize,
    pub grid: Grid,
}


pub fn conduction_map(
    grid: Grid,
    conductivity: &[f64],
    capacity: &[f64],
    spacing: f64,
    step: f64,
    periodic: [bool; 3],
    substeps: usize,
) -> CaeResult<ConductionMap> {
    let n = grid.cells();
    if conductivity.len() != n
        || capacity.len() != n
        || !conductivity.iter().chain(capacity).all(|v| f64::is_finite(*v))
        || conductivity.iter().any(|v| *v < 0.0)
        || capacity.iter().any(|v| *v <= 0.0)
    {
        return Err(err("Finite nonnegative conductivity and positive cell capacity required"));
    }
    if substeps < 1 {
        return Err(err("Positive fixed conduction substep count required"));
    }
    if !(spacing.is_finite() && spacing > 0.0 && step.is_finite() && step > 0.0) {
        return Err(err("Positive finite conduction spacing/time required"));
    }
    let mut links = vec![[0.0; 3]; n];
    for axis in 0..3 {
        let mut o = [0i64; 3];
        o[axis] = 1;
        for x in 0..n {
            let k = conductivity[x];
            let other = conductivity[grid.wrap(x, o)];
            let den = k + other;
            let mut g = 2.0 * k * other / if den > 0.0 { den } else { 1.0 } * spacing * step;
            if (!periodic[axis] || grid.shape[axis] == 1) && grid.coords(x)[axis] + 1 == grid.shape[axis] {
                g = 0.0;
            }
            links[x][axis] = g;
        }
    }
    let map = ConductionMap { links_dt: links, substeps, grid };
    validate(&map, capacity)?;
    Ok(map)
}


pub fn validate(map: &ConductionMap, capacity: &[f64]) -> CaeResult<()> {
    let n = map.grid.cells();
    if map.substeps < 1
        || map.links_dt.len() != n
        || map.links_dt.iter().flatten().any(|v| !v.is_finite() || *v < 0.0)
    {
        return Err(err("Invalid solid conduction schedule or link conductances"));
    }
    for x in 0..n {
        let mut outgoing = 0.0;
        for axis in 0..3 {
            let mut o = [0i64; 3];
            o[axis] = -1;
            outgoing += map.links_dt[x][axis] + map.links_dt[map.grid.wrap(x, o)][axis];
        }
        if outgoing > capacity[x] * map.substeps as f64 {
            return Err(err("Conduction substep exceeds the positive-capacity stability bound"));
        }
    }
    Ok(())
}

impl ConductionMap {
    fn substep(&self, t: &[f64], capacity: &[f64]) -> Vec<f64> {
        let grid = self.grid;
        let n = grid.cells();
        let s = self.substeps as f64;
        let mut change = vec![0.0; n];
        for axis in 0..3 {
            let mut fwd = [0i64; 3];
            fwd[axis] = 1;
            let flux: Vec<f64> =
                (0..n).map(|x| self.links_dt[x][axis] / s * (t[grid.wrap(x, fwd)] - t[x])).collect();
            let mut back = [0i64; 3];
            back[axis] = -1;
            for x in 0..n {
                change[x] = change[x] + flux[x] - flux[grid.wrap(x, back)];
            }
        }
        (0..n).map(|x| t[x] + change[x] / capacity[x]).collect()
    }

    #[must_use]
    pub fn advance(&self, temperature: &[f64], capacity: &[f64]) -> Vec<f64> {
        let mut t = temperature.to_vec();
        for _ in 0..self.substeps {
            t = self.substep(&t, capacity);
        }
        t
    }

    #[must_use]
    pub fn advance_vjp(&self, capacity: &[f64], bar: &[f64]) -> Vec<f64> {
        let grid = self.grid;
        let n = grid.cells();
        let s = self.substeps as f64;
        let mut b = bar.to_vec();
        for _ in 0..self.substeps {
            let mut out = b.clone();
            let c: Vec<f64> = (0..n).map(|x| b[x] / capacity[x]).collect();
            for axis in 0..3 {
                let mut fwd = [0i64; 3];
                fwd[axis] = 1;
                for x in 0..n {
                    let y = grid.wrap(x, fwd);
                    let flux_bar = c[x] - c[y];
                    let g = self.links_dt[x][axis] / s;
                    out[y] += g * flux_bar;
                    out[x] -= g * flux_bar;
                }
            }
            b = out;
        }
        b
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReservoirPatch {
    pub mask: Vec<bool>,
    pub conductivity: Vec<f64>,
    pub ambient_k: f64,
    pub coefficient_w_m2k: f64,
    pub emissivity: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReservoirMap {
    pub patches: Vec<ReservoirPatch>,
    pub spacing_m: f64,
    pub step_s: f64,
    pub temperature_unit_k: f64,
    pub energy_unit_j: f64,
}

pub const FACES: [&str; 6] = ["x-", "x+", "y-", "y+", "z-", "z+"];

fn on_face(grid: &Grid, x: usize, face: usize) -> bool {
    let axis = face / 2;
    let i = grid.coords(x)[axis];
    if face.is_multiple_of(2) { i == 0 } else { i + 1 == grid.shape[axis] }
}

const RESERVOIR_KEYS: [&str; 6] =
    ["id", "face", "mask", "ambient_temperature_K", "coefficient_W_m2K", "emissivity"];


pub fn reservoir_map(
    grid: Grid,
    patches: &Value,
    conductivity: &[f64],
    spacing: f64,
    step: f64,
    periodic: [bool; 3],
    temperature_unit: f64,
    energy_unit: f64,
) -> CaeResult<ReservoirMap> {
    let n = grid.cells();
    if conductivity.len() != n
        || !conductivity.iter().all(|v| f64::is_finite(*v))
        || conductivity.iter().any(|v| *v < 0.0)
    {
        return Err(err("Solid boundary conductivity must be a finite nonnegative grid"));
    }
    if [spacing, step, temperature_unit, energy_unit].iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return Err(err("Solid boundary scales must be positive finite scalars"));
    }
    let Some(list) = patches.as_array() else {
        return Err(err("Solid reservoir patches must be a list"));
    };
    let mut occupied = vec![vec![false; n]; 6];
    let mut seen = std::collections::BTreeSet::new();
    let mut rows = Vec::new();
    for p in list {
        let Some(m) =
            p.as_object().filter(|m| m.len() == 6 && RESERVOIR_KEYS.iter().all(|k| m.contains_key(*k)))
        else {
            return Err(err(
                "Solid reservoir requires id, face, mask, ambient_temperature_K, coefficient_W_m2K and emissivity",
            ));
        };
        let Some(id) = m["id"].as_str().filter(|s| !s.trim().is_empty() && !seen.contains(*s)) else {
            return Err(err("Solid reservoir IDs must be unique nonempty strings"));
        };
        seen.insert(id.to_string());
        let Some(face) = m["face"].as_str().and_then(|f| FACES.iter().position(|v| *v == f)) else {
            return Err(err("Solid reservoir face must be x-/x+/y-/y+/z-/z+"));
        };
        if periodic[face / 2] {
            return Err(err("Solid reservoir cannot lie on a periodic face"));
        }
        let mask_a = asarray(&m["mask"]);
        if mask_a.shape != grid.shape || !mask_a.is_bool() || !mask_a.data.iter().any(|v| *v != 0.0) {
            return Err(err("Solid reservoir requires nonempty boolean grid mask"));
        }
        let mask = mask_a.bools();
        if (0..n)
            .any(|x| mask[x] && (!on_face(&grid, x, face) || occupied[face][x] || conductivity[x] <= 0.0))
        {
            return Err(err(
                "Solid reservoir masks must be on-face, nonoverlapping and have positive conductivity",
            ));
        }
        let mut values = [0.0; 3];
        for (i, key) in ["ambient_temperature_K", "coefficient_W_m2K", "emissivity"].iter().enumerate() {
            match asarray(&m[*key]).real_scalar() {
                Some(v) => values[i] = v,
                None => return Err(err("Solid reservoir coefficients must be finite real scalars")),
            }
        }
        if values[0] <= 0.0 || values[1] < 0.0 || !(0.0..=1.0).contains(&values[2]) {
            return Err(err("Invalid solid reservoir temperature, convection or emissivity"));
        }
        for x in 0..n {
            occupied[face][x] |= mask[x];
        }
        rows.push(ReservoirPatch {
            conductivity: (0..n).map(|x| if mask[x] { conductivity[x] } else { 1.0 }).collect(),
            mask,
            ambient_k: values[0],
            coefficient_w_m2k: values[1],
            emissivity: values[2],
        });
    }
    Ok(ReservoirMap {
        patches: rows,
        spacing_m: spacing,
        step_s: step,
        temperature_unit_k: temperature_unit,
        energy_unit_j: energy_unit,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReservoirStep {
    pub temperature: Vec<f64>,
    pub convection_energy: f64,
    pub radiation_energy: f64,
    pub residual_k: f64,
}

impl ReservoirMap {
    fn surface(&self, p: &ReservoirPatch) -> Surface {
        Surface {
            spacing_m: self.spacing_m,
            ambient_k: p.ambient_k,
            emissivity: p.emissivity,
            convection_w_m2k: p.coefficient_w_m2k,
        }
    }

    fn powers<S: Scalar>(&self, x: usize, t: S) -> (S, S) {
        let mut qc = S::zero();
        let mut qr = S::zero();
        for p in &self.patches {
            if p.mask[x] {
                let e = exchange(t, S::from_f64(p.conductivity[x]), &self.surface(p));
                qc += e.convection_power_w;
                qr += e.radiation_power_w;
            }
        }
        (qc, qr)
    }

    fn residual<S: Scalar>(&self, x: usize, t: S, t0: f64, c: f64) -> S {
        let (qc, qr) = self.powers(x, t);
        t - t0 - (qc + qr) * self.step_s / c
    }

    #[must_use]
    pub fn advance(&self, temperature: &[f64], capacity: &[f64]) -> ReservoirStep {
        let unit = self.temperature_unit_k;
        let energy = self.energy_unit_j;
        let n = temperature.len();
        let mut end = vec![0.0; n];
        let mut residual: f64 = 0.0;
        let (mut qc_total, mut qr_total) = (0.0, 0.0);
        for x in 0..n {
            let t0 = temperature[x] * unit;
            let c = capacity[x] * energy / unit;
            let mut lo = t0;
            let mut hi = t0;
            for p in &self.patches {
                if p.mask[x] {
                    lo = lo.min(p.ambient_k);
                    hi = hi.max(p.ambient_k);
                }
            }
            for _ in 0..60 {
                let mid = 0.5 * (lo + hi);
                if self.residual(x, mid, t0, c) > 0.0 {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            let t = 0.5 * (lo + hi);
            let (qc, qr) = self.powers(x, t);
            qc_total += qc;
            qr_total += qr;
            residual = residual.max(self.residual(x, t, t0, c).abs());
            end[x] = t / unit;
        }
        ReservoirStep {
            temperature: end,
            convection_energy: qc_total * self.step_s / energy,
            radiation_energy: qr_total * self.step_s / energy,
            residual_k: residual,
        }
    }

    #[must_use]
    pub fn advance_vjp(
        &self,
        temperature: &[f64],
        capacity: &[f64],
        end: &[f64],
        t_bar: &[f64],
        energy_bar: [f64; 2],
    ) -> Vec<f64> {
        let unit = self.temperature_unit_k;
        let energy = self.energy_unit_j;
        let scale = [energy_bar[0] * self.step_s / energy, energy_bar[1] * self.step_s / energy];
        (0..temperature.len())
            .map(|x| {
                let t0 = temperature[x] * unit;
                let c = capacity[x] * energy / unit;
                let t = end[x] * unit;
                let r = self.residual(x, Dual::<1>::variable(t, 0), t0, c);
                let dt = 1.0 / r.eps[0];
                let (qc, qr) = self.powers(x, Dual::<1>::variable(t, 0));
                let end_bar = t_bar[x] / unit + scale[0] * qc.eps[0] + scale[1] * qr.eps[0];
                end_bar * dt * unit
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WallFlux {
    pub indices: Vec<usize>,
    pub cell_energy: Vec<f64>,
    pub face_energy: [f64; 6],
}


pub fn wall_flux_map(
    patches: &Value,
    grid: Grid,
    periodic: [bool; 3],
    spacing: f64,
    step: f64,
    energy_unit: f64,
    open_reservoirs: bool,
) -> CaeResult<WallFlux> {
    let Some(list) = patches.as_array() else {
        return Err(err("wall_heat_flux_patches must be a list"));
    };
    let n = grid.cells();
    let mut seen = std::collections::BTreeSet::new();
    let mut occupied = vec![vec![false; n]; 6];
    let mut heat = vec![0.0; n];
    let mut faces = [0.0; 6];
    for p in list {
        let Some(m) = p.as_object().filter(|m| {
            m.len() == 4 && ["id", "face", "mask", "inward_flux_W_m2"].iter().all(|k| m.contains_key(*k))
        }) else {
            return Err(err("Wall heat patch requires id, face, mask and inward_flux_W_m2"));
        };
        let Some(id) = m["id"].as_str().filter(|s| !s.trim().is_empty() && !seen.contains(*s)) else {
            return Err(err("Wall heat patch IDs must be unique nonempty strings"));
        };
        seen.insert(id.to_string());
        let Some(index) = m["face"].as_str().and_then(|f| FACES.iter().position(|v| *v == f)) else {
            return Err(err("Wall heat face must be x-/x+/y-/y+/z-/z+"));
        };
        if periodic[index / 2] || open_reservoirs {
            return Err(err("Wall heat cannot be applied on periodic or reservoir faces"));
        }
        let mask_a = asarray(&m["mask"]);
        if mask_a.shape != grid.shape || !mask_a.is_bool() || !mask_a.data.iter().any(|v| *v != 0.0) {
            return Err(err("Wall heat mask must be a nonempty grid-shaped boolean array"));
        }
        let mask = mask_a.bools();
        if (0..n).any(|x| mask[x] && (!on_face(&grid, x, index) || occupied[index][x])) {
            return Err(err("Wall heat masks must lie on their face and not overlap on that face"));
        }
        let Some(flux) = asarray(&m["inward_flux_W_m2"]).real_scalar() else {
            return Err(err("Wall heat flux must be a finite scalar"));
        };
        let mut face_total = 0.0;
        for x in 0..n {
            occupied[index][x] |= mask[x];
            let e = if mask[x] { 1.0 } else { 0.0 } * flux * (spacing * spacing) * step / energy_unit;
            heat[x] += e;
            face_total += e;
        }
        faces[index] += face_total;
    }
    let indices: Vec<usize> = (0..n).filter(|x| heat[*x] != 0.0).collect();
    Ok(WallFlux { cell_energy: indices.iter().map(|x| heat[*x]).collect(), indices, face_energy: faces })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViscosityLaw {
    pub reference: f64,
    pub temperature: f64,
    pub exponent: f64,
    pub minimum: f64,
    pub maximum: f64,
}

impl ViscosityLaw {

    pub fn normalize(config: &Value, spacing: f64, step: f64, temperature_unit: f64) -> CaeResult<Self> {
        const KEYS: [&str; 5] = [
            "reference_m2_s",
            "reference_temperature_K",
            "exponent",
            "minimum_temperature_K",
            "maximum_temperature_K",
        ];
        let Some(m) = config.as_object().filter(|m| m.len() == 5 && KEYS.iter().all(|k| m.contains_key(*k)))
        else {
            return Err(err(
                "Viscosity requires reference_m2_s, reference_temperature_K, exponent and temperature limits",
            ));
        };
        let mut v = [0.0; 5];

        for (i, key) in KEYS.iter().enumerate() {
            match asarray(&m[*key]).real_scalar() {
                Some(x) if *key == "exponent" || x > 0.0 => v[i] = x,
                _ => return Err(err(format!("Invalid viscosity {key}"))),
            }
        }
        if v[3] >= v[4] {
            return Err(err("Viscosity temperature limits must be increasing"));
        }
        let law = Self {
            reference: v[0] * step / (spacing * spacing),
            temperature: v[1] / temperature_unit,
            exponent: v[2],
            minimum: v[3] / temperature_unit,
            maximum: v[4] / temperature_unit,
        };
        for t in [law.minimum, law.maximum] {
            let tau = law.relaxation(t);
            if !tau.is_finite() || tau <= 0.5 || tau > 1.0 / 1.35 {
                return Err(err(
                    "Viscosity law exceeds the admitted relaxation range within its temperature limits",
                ));
            }
        }
        Ok(law)
    }

    pub fn relaxation<S: Scalar>(&self, t: S) -> S {
        (t / self.temperature).powf(self.exponent) * self.reference / t + 0.5
    }


    pub fn admit_temperature(&self, minimum: f64, maximum: f64) -> CaeResult<()> {
        if !minimum.is_finite() || !maximum.is_finite() || minimum < self.minimum || maximum > self.maximum {
            return Err(err("Gas temperature is outside the authored viscosity validity interval"));
        }
        Ok(())
    }
}

