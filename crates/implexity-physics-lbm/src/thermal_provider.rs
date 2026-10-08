// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use implexity_ad::{AdError, Dual, Tape, Var};
use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, ProviderCapabilities, ProviderProblem, Sensitivity,
};
use implexity_core::coupling_graph::{CouplingDeclaration, CouplingEdge};
use implexity_core::orchestration::PublishedContract;
use implexity_core::{CaeError, CaeResult};
use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use crate::ageing;
use crate::d3q19::{Grid, POSITIVE, Q};
use crate::design_ops::{
    Design, DesignSensitivities, LbmOperations, array, check_responses, presentation, single_coordinate,
};
use crate::geometry;
use crate::nparray::{NdArray, asarray, py_int, to_nested};
use crate::provider::{
    self, COORDINATE, LatticeBoltzmannProvider, contract, descriptor, insert_wall_fields, missing_method,
    problem_value,
};
use crate::radiation::{Surface, exchange as surface_exchange};
use crate::solver::{LbmProblem, Tau, VECTOR_LEN};
use crate::thermal_links::{
    EnergyInputs, RollIndex, energy_step, energy_step_tape, mass_rates_from_post, mass_rates_from_post_vjp,
};
use crate::wall_exchange;

pub const NAME: &str = "lattice_boltzmann_thermal_d3q19";
pub const RESPONSES: [&str; 9] = [
    "mean_velocity_x_m_s",
    "mean_velocity_y_m_s",
    "mean_velocity_z_m_s",
    "kinetic_energy_J",
    "porous_dissipation_W",
    "fluid_volume_m3",
    "mean_temperature_K",
    "temperature_variance_K2",
    "stored_thermal_energy_J",
];
pub const UNITS: [&str; 9] = ["m/s", "m/s", "m/s", "J", "W", "m^3", "K", "K^2", "J"];

pub const NOTES: [&str; 8] = [
    "Experimental LBM sensible heat transport with fixed-solid conduction and optional temperature-dependent viscosity.",
    "solid_conductivity_W_mK accepts a positive scalar or exact cell-shaped fixed material map; maps are not temperature-dependent tables or material-design coordinates.",
    "Density changes advect fluid heat capacity; porous design changes drag and effective conductivity, not fluid storage fraction.",
    "Optional thermal.contact_resistance_m2K_W is a nonnegative shape+(3,) array on positive x/y/z internal faces; zero means perfect contact. It changes conduction, not mass permeability.",
    "Temperature updates viscosity at the next flow interval; first-order staggered feedback, not an implicit converged coupling solve.",
    "Optional radiation_patches add gray-surface exchange to a large isothermal reservoir, with emissivity, coefficient_W_m2K for simultaneous convection, ambient_temperature_K, id, face and mask. Half-cell surface balance and its implicit derivative are solved per substep.",
    "No buoyancy, participating-medium radiation, boiling or viscous heating; thermal expansion and mechanics require the structural provider.",
    "Fixed transient schedule and authored substeps; not steady-state or material qualification.",
];

const THERMAL_KEYS: [&str; 11] = [
    "fluid_conductivity_W_mK",
    "fluid_specific_heat_J_kgK",
    "history_byte_budget",
    "initial_temperature_K",
    "maximum_temperature_K",
    "minimum_temperature_K",
    "provenance",
    "solid_capacity_J_m3K",
    "solid_conductivity_W_mK",
    "source_W_m3",
    "substeps",
];
const THERMAL_OPTIONAL: [&str; 6] = [
    "viscosity_feedback",
    "port_temperatures_K",
    "heat_flux_patches",
    "convection_patches",
    "contact_resistance_m2K_W",
    "radiation_patches",
];

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConvectionPatch {
    pub mask: Vec<bool>,
    pub coefficient_w_m2k: f64,
    pub ambient_k: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RadiationPatch {
    pub mask: Vec<bool>,
    pub coefficient_w_m2k: f64,
    pub ambient_k: f64,
    pub emissivity: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThermalSettings {
    pub initial_temperature_k: Vec<f64>,
    pub fluid_specific_heat: f64,
    pub solid_capacity: Vec<f64>,
    pub fluid_conductivity: f64,
    pub solid_conductivity: Vec<f64>,
    pub source_w_m3: Vec<f64>,
    pub minimum_temperature_k: f64,
    pub maximum_temperature_k: f64,
    pub substeps: usize,
    pub history_byte_budget: i64,
    pub provenance: String,
    pub viscosity_feedback: (f64, f64),
    pub port_temperatures_k: Vec<f64>,
    pub contact_resistance: Vec<[f64; 3]>,
    pub heat_flux_power_w: Vec<f64>,
    pub convection_patches: Vec<ConvectionPatch>,
    pub radiation_patches: Vec<RadiationPatch>,
}

#[derive(Clone, Debug)]
pub struct ThermalProblem {
    pub p: LbmProblem,
    pub t: ThermalSettings,
}

fn scalar_positive(t: &Map<String, Value>, key: &str) -> CaeResult<f64> {
    match asarray(t.get(key).unwrap_or(&Value::Null)).real_scalar() {
        Some(v) if v > 0.0 => Ok(v),
        _ => Err(err(format!("{key} must be a positive finite scalar"))),
    }
}

fn is_face(face: &str) -> Option<(usize, bool)> {
    let axis = match face.as_bytes().first() {
        Some(b'x') => 0,
        Some(b'y') => 1,
        Some(b'z') => 2,
        _ => return None,
    };
    match &face[1..] {
        "min" => Some((axis, true)),
        "max" => Some((axis, false)),
        _ => None,
    }
}

impl ThermalProblem {

    pub fn normalize(problem: &Value) -> CaeResult<Self> {
        let keys_ok = problem.as_object().is_some_and(|m| {
            let mut keys: Vec<&str> = m
                .keys()
                .map(String::as_str)
                .filter(|k| *k != "field_registration" && *k != "environmental_ageing")
                .collect();
            keys.sort_unstable();
            keys == ["flow", "thermal"]
        });
        let Some(root) = problem.as_object().filter(|_| keys_ok) else {
            return Err(err("Coupled LBM requires flow and thermal"));
        };
        let p = LbmProblem::normalize(&root["flow"])?;
        geometry::check_registration(root, p.shape(), p.spacing_m, p.origin_m)?;
        let t_ok = root["thermal"].as_object().is_some_and(|m| {
            let mut keys: Vec<&str> =
                m.keys().map(String::as_str).filter(|k| !THERMAL_OPTIONAL.contains(k)).collect();
            keys.sort_unstable();
            keys == THERMAL_KEYS
        });
        let Some(t) = root["thermal"].as_object().filter(|_| t_ok) else {
            let list: Vec<String> = THERMAL_KEYS.iter().map(|k| format!("'{k}'")).collect();
            return Err(err(format!("Thermal problem requires [{}]", list.join(", "))));
        };
        let shape = p.shape();
        let n = p.cells();
        let mut grids = BTreeMap::new();
        for key in ["initial_temperature_K", "solid_capacity_J_m3K", "source_W_m3"] {
            let a = asarray(&t[key]);
            if a.shape != shape || !a.is_real() || !a.all_finite() {
                return Err(err(format!("{key} requires finite shape-matched values")));
            }
            if key != "source_W_m3" && a.data.iter().any(|v| *v <= 0.0) {
                return Err(err(format!("{key} must be positive")));
            }
            grids.insert(key, a.data);
        }
        let solid_k = asarray(&t["solid_conductivity_W_mK"]);
        if !(solid_k.shape.is_empty() || solid_k.shape == shape)
            || !solid_k.is_real()
            || !solid_k.all_finite()
            || solid_k.data.iter().any(|v| *v <= 0.0)
        {
            return Err(err(
                "solid_conductivity_W_mK requires a positive finite scalar or exact shape-matched grid",
            ));
        }
        let solid_conductivity =
            if solid_k.shape.is_empty() { vec![solid_k.data[0]; n] } else { solid_k.data };
        let cp = scalar_positive(t, "fluid_specific_heat_J_kgK")?;
        let kf = scalar_positive(t, "fluid_conductivity_W_mK")?;
        let t_min = scalar_positive(t, "minimum_temperature_K")?;
        let t_max = scalar_positive(t, "maximum_temperature_K")?;
        let initial = grids.remove("initial_temperature_K").unwrap_or_default();
        if t_min >= t_max || initial.iter().any(|v| *v < t_min || *v > t_max) {
            return Err(err("Initial temperature must lie inside the authored material validity interval"));
        }
        let substeps = py_int(&t["substeps"]).filter(|s| (1..=10_000).contains(s));
        let Some(substeps) = substeps else {
            return Err(err("Thermal substeps must be an integer in [1,10000]"));
        };
        let required = (p.step_count as i128 + 1) * n as i128 * 3 * 8;
        let Some(budget) = py_int(&t["history_byte_budget"]).filter(|b| i128::from(*b) >= required) else {
            return Err(err("Thermal output history exceeds budget; differentiation memory is additional"));
        };
        let Some(provenance) = t["provenance"].as_str().filter(|s| !s.trim().is_empty()) else {
            return Err(err("Thermal material provenance is required"));
        };
        let default_law = json!({"coefficient_per_K": 0.0, "reference_temperature_K": 300.0});
        let law = t.get("viscosity_feedback").unwrap_or(&default_law);
        let Some(law_map) = law.as_object().filter(|m| {
            m.len() == 2 && m.contains_key("coefficient_per_K") && m.contains_key("reference_temperature_K")
        }) else {
            return Err(err("Viscosity feedback requires coefficient_per_K and reference_temperature_K"));
        };
        for value in law_map.values() {
            if asarray(value).real_scalar().is_none() {
                return Err(err("Finite scalar viscosity feedback parameters required"));
            }
        }
        let beta = asarray(&law_map["coefficient_per_K"]).data[0];
        let reference = asarray(&law_map["reference_temperature_K"]).data[0];
        if reference <= 0.0 {
            return Err(err("Positive viscosity reference temperature required"));
        }
        let exponents = [beta * (t_min - reference), beta * (t_max - reference)];
        if exponents.iter().any(|e| e.abs() > 50.0) {
            return Err(err("Viscosity law exceeds admitted exponential range"));
        }
        let taus = exponents.map(|e| 0.5 + (p.tau - 0.5) * e.exp());
        if taus.iter().any(|v| *v < 0.51) || taus.iter().any(|v| *v > 2.0) {
            return Err(err(
                "Viscosity law must retain relaxation time in [0.51,2] over the full material interval",
            ));
        }
        let reservoirs = t.get("port_temperatures_K").cloned().unwrap_or_else(|| json!({}));
        let ids: std::collections::BTreeSet<&str> = p.ports.iter().map(|q| q.port.id.as_str()).collect();
        let res_ok = reservoirs
            .as_object()
            .is_some_and(|m| m.len() == ids.len() && m.keys().all(|k| ids.contains(k.as_str())));
        let Some(res_map) = reservoirs.as_object().filter(|_| res_ok) else {
            return Err(err(
                "port_temperatures_K must specify every port, including possible outlet backflow",
            ));
        };
        for value in res_map.values() {
            match asarray(value).real_scalar() {
                Some(v) if t_min <= v && v <= t_max => {}
                _ => {
                    return Err(err(
                        "Port reservoir temperatures must lie in the material validity interval",
                    ));
                }
            }
        }
        let port_temperatures_k: Vec<f64> =
            p.ports.iter().map(|q| asarray(&res_map[q.port.id.as_str()]).data[0]).collect();
        let contact_raw = match t.get("contact_resistance_m2K_W") {
            Some(v) => asarray(v),
            None => NdArray {
                shape: vec![shape[0], shape[1], shape[2], 3],
                kind: crate::nparray::Kind::Float,
                data: vec![0.0; 3 * n],
            },
        };
        if contact_raw.shape != [shape[0], shape[1], shape[2], 3]
            || !contact_raw.is_real()
            || !contact_raw.all_finite()
            || contact_raw.data.iter().any(|v| *v < 0.0)
        {
            return Err(err(
                "contact_resistance_m2K_W requires finite nonnegative shape+(3,) positive-face values",
            ));
        }
        let contact: Vec<[f64; 3]> = (0..n)
            .map(|x| [contact_raw.data[3 * x], contact_raw.data[3 * x + 1], contact_raw.data[3 * x + 2]])
            .collect();
        for axis in 0..3 {
            if !p.periodic_axes[axis]
                && (0..n).any(|x| p.grid.coords(x)[axis] + 1 == shape[axis] && contact[x][axis] != 0.0)
            {
                return Err(err(
                    "Contact resistance on a closed outer face must be zero; use a wall boundary patch",
                ));
            }
        }
        let empty = json!([]);
        let lists: Vec<(&str, &Value)> = vec![
            ("flux", t.get("heat_flux_patches").unwrap_or(&empty)),
            ("convection", t.get("convection_patches").unwrap_or(&empty)),
            ("radiation", t.get("radiation_patches").unwrap_or(&empty)),
        ];
        for (kind, list) in &lists {
            if !list.is_array() {
                let key = match *kind {
                    "flux" => "heat_flux_patches",
                    "convection" => "convection_patches",
                    _ => "radiation_patches",
                };
                return Err(err(format!("{key} must be a list")));
            }
        }
        let dx = p.spacing_m;
        let mut power = vec![0.0; n];
        let mut used: BTreeMap<String, Vec<bool>> = BTreeMap::new();
        let mut seen_ids = std::collections::BTreeSet::new();
        let mut convection_patches = Vec::new();
        let mut radiation_patches = Vec::new();
        for (kind, list) in &lists {
            for patch in list.as_array().into_iter().flatten() {
                let fields: Vec<&str> = match *kind {
                    "flux" => vec!["inward_flux_W_m2"],
                    "convection" => vec!["coefficient_W_m2K", "ambient_temperature_K"],
                    _ => vec!["coefficient_W_m2K", "ambient_temperature_K", "emissivity"],
                };
                let ok = patch.as_object().is_some_and(|m| {
                    m.len() == 3 + fields.len()
                        && ["id", "face", "mask"].iter().chain(fields.iter()).all(|k| m.contains_key(*k))
                });
                let Some(m) = patch.as_object().filter(|_| ok) else {
                    return Err(err(
                        "Thermal patch requires id, face, mask and its explicit flux or convection parameters",
                    ));
                };
                let Some(name) = m["id"].as_str().filter(|s| !s.is_empty() && !seen_ids.contains(*s)) else {
                    return Err(err("Unique nonempty heat-flux patch ids required"));
                };
                seen_ids.insert(name.to_string());
                let Some((axis, low)) = m["face"].as_str().and_then(is_face) else {
                    return Err(err("Unknown thermal face"));
                };
                let face = m["face"].as_str().unwrap_or_default().to_string();
                if p.periodic_axes[axis] {
                    return Err(err("Heat-flux patches cannot be placed on periodic faces"));
                }
                let mask_a = asarray(&m["mask"]);
                let on_face = |x: usize| {
                    let i = p.grid.coords(x)[axis];
                    if low { i == 0 } else { i + 1 == shape[axis] }
                };
                if mask_a.shape != shape
                    || !mask_a.is_bool()
                    || !mask_a.data.iter().any(|v| *v != 0.0)
                    || (0..n).any(|x| mask_a.data[x] != 0.0 && !on_face(x))
                {
                    return Err(err("Heat-flux mask must be nonempty and confined to its outer face"));
                }
                let mask = mask_a.bools();
                let face_used = used.entry(face.clone()).or_insert_with(|| vec![false; n]);
                if (0..n).any(|x| mask[x] && face_used[x]) {
                    return Err(err("Heat-flux patches overlap on the same face"));
                }
                for x in 0..n {
                    face_used[x] |= mask[x];
                }
                if p.ports.iter().any(|q| {
                    q.port.axis == axis && q.port.face == face && (0..n).any(|x| mask[x] && q.port.mask[x])
                }) {
                    return Err(err(
                        "A face cannot simultaneously be a flow port and prescribed wall heat-flux patch",
                    ));
                }
                let mut values = BTreeMap::new();
                for field in &fields {
                    match asarray(&m[*field]).real_scalar() {
                        Some(v) => {
                            values.insert(*field, v);
                        }
                        None => return Err(err("Thermal boundary parameters must be finite real scalars")),
                    }
                }
                if *kind == "flux" {
                    let q = values["inward_flux_W_m2"];
                    for x in 0..n {
                        power[x] += if mask[x] { 1.0 } else { 0.0 } * q * (dx * dx);
                    }
                } else {
                    let h = values["coefficient_W_m2K"];
                    let ambient = values["ambient_temperature_K"];
                    if h < 0.0 || !(t_min <= ambient && ambient <= t_max) {
                        return Err(err(
                            "Nonnegative convection coefficient and ambient temperature inside material validity required",
                        ));
                    }
                    if *kind == "radiation" {
                        let e = values["emissivity"];
                        if !(0.0..=1.0).contains(&e) {
                            return Err(err("Radiation emissivity must lie in [0,1]"));
                        }
                        radiation_patches.push(RadiationPatch {
                            mask,
                            coefficient_w_m2k: h,
                            ambient_k: ambient,
                            emissivity: e,
                        });
                    } else {
                        convection_patches.push(ConvectionPatch {
                            mask,
                            coefficient_w_m2k: h,
                            ambient_k: ambient,
                        });
                    }
                }
            }
        }
        if !power.iter().all(|v| v.is_finite()) || !power.iter().sum::<f64>().is_finite() {
            return Err(err("Prescribed heat power exceeds finite numerical range"));
        }
        if let Some(spec) = root.get("environmental_ageing").filter(|v| !v.is_null()) {
            let (cells, indices) = solid_indices(&p.solid_mask);
            let times = step_times(p.step_count, p.step_s);
            let cells: Vec<f64> = cells.iter().map(|&i| initial[i]).collect();
            ageing::admit_initial(spec, p.shape(), &times, Some(&indices), &cells)?;
        }
        Ok(Self {
            p,
            t: ThermalSettings {
                initial_temperature_k: initial,
                fluid_specific_heat: cp,
                solid_capacity: grids.remove("solid_capacity_J_m3K").unwrap_or_default(),
                fluid_conductivity: kf,
                solid_conductivity,
                source_w_m3: grids.remove("source_W_m3").unwrap_or_default(),
                minimum_temperature_k: t_min,
                maximum_temperature_k: t_max,
                substeps: usize::try_from(substeps).unwrap_or(1),
                history_byte_budget: budget,
                provenance: provenance.to_string(),
                viscosity_feedback: (beta, reference),
                port_temperatures_k,
                contact_resistance: contact,
                heat_flux_power_w: power,
                convection_patches,
                radiation_patches,
            },
        })
    }
}

fn solid_indices(mask: &[bool]) -> (Vec<usize>, Vec<i64>) {
    let cells: Vec<usize> = mask.iter().enumerate().filter(|(_, m)| **m).map(|(i, _)| i).collect();
    let indices = cells.iter().map(|&i| i64::try_from(i).unwrap_or(i64::MAX)).collect();
    (cells, indices)
}

fn step_times(steps: usize, step_s: f64) -> Vec<f64> {
    (0..=steps).map(|n| n as f64 * step_s).collect()
}

#[derive(Clone, Debug)]
pub struct ThermalModel {
    pub phi: Vec<f64>,
    pub alpha: Vec<f64>,
    pub k: Vec<f64>,
    pub wall_g: Vec<f64>,
    pub wall_gt: Vec<f64>,
    pub reservoir: Vec<f64>,
    pub source: Vec<f64>,
    pub rolls: RollIndex,
}

#[derive(Clone, Debug, Default)]
pub struct IntervalRecords {
    pub maximum_outgoing: Vec<f64>,
    pub minimum_temperature: Vec<f64>,
    pub maximum_temperature: Vec<f64>,
    pub minimum_capacity: Vec<f64>,
    pub boundary_mass: Vec<f64>,
    pub boundary_energy: Vec<f64>,
    pub convection_energy: Vec<f64>,
    pub radiation_energy: Vec<f64>,
    pub radiation_residual: Vec<f64>,
}

#[derive(Clone, Debug)]
pub struct Trajectory {
    pub f: Vec<Vec<f64>>,
    pub temperature: Vec<Vec<f64>>,
    pub capacity: Vec<Vec<f64>>,
    pub records: IntervalRecords,
}

struct FlowDrive {
    after: Vec<f64>,
    rates: Vec<Vec<f64>>,
    exchange: Vec<f64>,
    exchanged_density: f64,
}

impl ThermalProblem {
    #[must_use]
    pub fn relaxation(&self, temperature: &[f64]) -> Vec<f64> {
        let (beta, reference) = self.t.viscosity_feedback;
        temperature.iter().map(|v| 0.5 + (self.p.tau - 0.5) * (beta * (v - reference)).exp()).collect()
    }

    fn relaxation_derivative(&self, temperature: &[f64]) -> Vec<f64> {
        let (beta, reference) = self.t.viscosity_feedback;
        temperature.iter().map(|v| (self.p.tau - 0.5) * (beta * (v - reference)).exp() * beta).collect()
    }

    #[must_use]
    pub fn model(&self, raw: &[f64]) -> ThermalModel {
        let p = &self.p;
        let t = &self.t;
        let n = p.cells();
        let phi = p.fraction(raw);
        let alpha = p.resistance(&phi);
        let k: Vec<f64> = (0..n)
            .map(|x| phi[x] * t.fluid_conductivity + (1.0 - phi[x]) * t.solid_conductivity[x])
            .collect();
        let dx = p.spacing_m;
        let mut wall_g = vec![0.0; n];
        let mut wall_gt = vec![0.0; n];
        for patch in &t.convection_patches {
            let h = patch.coefficient_w_m2k;
            if h == 0.0 {
                continue;
            }
            for x in 0..n {
                let m = if patch.mask[x] { 1.0 } else { 0.0 };
                let g = m * (dx * dx) * k[x] / (k[x] / h + 0.5 * dx);
                wall_g[x] += g;
                wall_gt[x] += g * patch.ambient_k;
            }
        }
        let mut reservoir = t.initial_temperature_k.clone();
        for (port, temp) in p.ports.iter().zip(&t.port_temperatures_k) {
            for &x in &port.port.cells {
                reservoir[x] = *temp;
            }
        }
        let dv = dx.powi(3);
        let source: Vec<f64> = (0..n).map(|x| t.source_w_m3[x] * dv + t.heat_flux_power_w[x]).collect();
        ThermalModel { phi, alpha, k, wall_g, wall_gt, reservoir, source, rolls: RollIndex::new(&p.grid) }
    }

    #[must_use]
    pub fn initial(&self) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let p = &self.p;
        let f0 = p.initial_populations();
        let dv = p.spacing_m.powi(3);
        let cp = self.t.fluid_specific_heat;
        let capacity = (0..p.cells())
            .map(|x| {
                let c = if p.solid_mask[x] {
                    self.t.solid_capacity[x]
                } else {
                    let mut rho = f0[x * Q];
                    for q in 1..Q {
                        rho += f0[x * Q + q];
                    }
                    rho * p.density_kg_m3 * cp
                };
                c * dv
            })
            .collect();
        (f0, self.t.initial_temperature_k.clone(), capacity)
    }

    fn inputs<'a>(&'a self, m: &'a ThermalModel, exchange: &'a [f64]) -> EnergyInputs<'a> {
        EnergyInputs {
            grid: self.p.grid,
            periodic: self.p.periodic_axes,
            spacing_m: self.p.spacing_m,
            step_s: self.p.step_s / self.t.substeps as f64,
            cp: self.t.fluid_specific_heat,
            contact: Some(&self.t.contact_resistance),
            source_w: &m.source,
            boundary_mass_rate: exchange,
            reservoir: &m.reservoir,
        }
    }

    fn drive(&self, m: &ThermalModel, before: &[f64], temperature: &[f64]) -> FlowDrive {
        let p = &self.p;
        let tau = self.relaxation(temperature);
        let interval = p.interval(before, &m.alpha, Tau::PerCell(&tau));
        let rates = mass_rates_from_post(p, &interval.post);
        let scale = p.density_kg_m3 * p.spacing_m.powi(3) / p.step_s;
        let mut exchange = vec![0.0; p.cells()];
        for (cell, delta) in &interval.port_exchange {
            exchange[*cell] = delta * scale;
        }
        FlowDrive { after: interval.after, rates, exchange, exchanged_density: interval.exchanged_density }
    }

    fn substep(
        &self,
        m: &ThermalModel,
        drive: &FlowDrive,
        temperature: &[f64],
        capacity: &[f64],
    ) -> (Vec<f64>, Vec<f64>, f64, [f64; 7]) {
        let n = self.p.cells();
        let dt = self.p.step_s / self.t.substeps as f64;
        let mut g = m.wall_g.clone();
        let mut gt = m.wall_gt.clone();
        let mut rad = vec![0.0; n];
        let mut correction = vec![0.0; n];
        let mut residual: f64 = 0.0;
        for patch in &self.t.radiation_patches {
            let s = Surface {
                spacing_m: self.p.spacing_m,
                ambient_k: patch.ambient_k,
                emissivity: patch.emissivity,
                convection_w_m2k: patch.coefficient_w_m2k,
            };
            let mut worst: f64 = 0.0;
            for x in 0..n {
                if !patch.mask[x] {
                    continue;
                }
                let e = surface_exchange(temperature[x], m.k[x], &s);
                g[x] += e.conductance_w_k;
                gt[x] += e.reservoir_power_w;
                rad[x] += e.radiation_power_w;
                correction[x] += e.outgoing_correction_w_k;
                worst = worst.max(e.surface_residual_k.abs());
            }
            residual = residual.max(worst);
        }
        let inputs = self.inputs(m, &drive.exchange);
        let out = energy_step(&inputs, temperature, capacity, &m.k, &drive.rates, &g, &gt);
        let mut outgoing: f64 = f64::NEG_INFINITY;
        for x in 0..n {
            outgoing = outgoing.max(out.outgoing_fraction[x] + dt * correction[x] / capacity[x]);
        }
        let min_t = out.temperature_k.iter().fold(f64::INFINITY, |a, v| a.min(*v));
        let max_t = out.temperature_k.iter().fold(f64::NEG_INFINITY, |a, v| a.max(*v));
        let min_c = out.capacity_j_k.iter().fold(f64::INFINITY, |a, v| a.min(*v));
        let boundary: f64 = out.boundary_power_w.iter().sum::<f64>() * dt;
        let convection: f64 = (0..n).map(|x| out.wall_convection_power_w[x] - rad[x]).sum::<f64>() * dt;
        let radiation: f64 = rad.iter().sum::<f64>() * dt;
        (
            out.temperature_k,
            out.capacity_j_k,
            outgoing,
            [min_t, max_t, min_c, boundary, convection, radiation, residual],
        )
    }

    #[must_use]
    pub fn trajectory(&self, m: &ThermalModel) -> Trajectory {
        let (f0, t0, c0) = self.initial();
        let steps = self.p.step_count;
        let mut f = Vec::with_capacity(steps + 1);
        let mut temperature = Vec::with_capacity(steps + 1);
        let mut capacity = Vec::with_capacity(steps + 1);
        f.push(f0);
        temperature.push(t0);
        capacity.push(c0);
        let mut r = IntervalRecords::default();
        for step in 0..steps {
            let drive = self.drive(m, &f[step], &temperature[step]);
            let mut temp = temperature[step].clone();
            let mut cap = capacity[step].clone();
            let mut worst: f64 = 0.0;
            let mut ext = [f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, 0.0, 0.0, 0.0, f64::NEG_INFINITY];
            for _ in 0..self.t.substeps {
                let (t1, c1, outgoing, e) = self.substep(m, &drive, &temp, &cap);
                worst = worst.max(outgoing);
                ext[0] = ext[0].min(e[0]);
                ext[1] = ext[1].max(e[1]);
                ext[2] = ext[2].min(e[2]);
                ext[3] += e[3];
                ext[4] += e[4];
                ext[5] += e[5];
                ext[6] = ext[6].max(e[6]);
                temp = t1;
                cap = c1;
            }
            r.maximum_outgoing.push(worst);
            r.minimum_temperature.push(ext[0]);
            r.maximum_temperature.push(ext[1]);
            r.minimum_capacity.push(ext[2]);
            r.boundary_mass.push(drive.exchanged_density);
            r.boundary_energy.push(ext[3]);
            r.convection_energy.push(ext[4]);
            r.radiation_energy.push(ext[5]);
            r.radiation_residual.push(ext[6]);
            f.push(drive.after);
            temperature.push(temp);
            capacity.push(cap);
        }
        Trajectory { f, temperature, capacity, records: r }
    }
}

#[derive(Clone, Debug)]
pub struct ThermalSolved {
    pub tp: ThermalProblem,
    pub raw: Vec<f64>,
    pub model: ThermalModel,
    pub trajectory: Trajectory,
    pub diagnostics: Map<String, Value>,
}

fn max_of(v: &[f64]) -> f64 {
    v.iter().fold(f64::NEG_INFINITY, |a, b| a.max(*b))
}

fn min_of(v: &[f64]) -> f64 {
    v.iter().fold(f64::INFINITY, |a, b| a.min(*b))
}

impl ThermalProblem {

    pub fn admit(&self, model: &ThermalModel, traj: &Trajectory) -> CaeResult<Map<String, Value>> {
        let p = &self.p;
        let t = &self.t;
        let history = crate::solver::History {
            states: traj.f.clone(),
            boundary_mass_changes_lattice: traj.records.boundary_mass.clone(),
        };
        let mut d = p.admit(&model.alpha, &history, true)?;
        d.insert("field_metadata".into(), geometry::field_metadata(p.shape(), p.spacing_m, p.origin_m)?);
        if let Some(s) = &p.geometry_sampling {
            d.insert("geometry_sampling".into(), s.clone());
        }
        let r = &traj.records;
        let finite = |v: &[f64]| v.iter().all(|x| x.is_finite());
        if !traj.temperature.iter().all(|v| finite(v))
            || !traj.capacity.iter().all(|v| finite(v))
            || ![
                &r.maximum_outgoing,
                &r.minimum_temperature,
                &r.maximum_temperature,
                &r.minimum_capacity,
                &r.radiation_energy,
                &r.radiation_residual,
            ]
            .iter()
            .all(|v| finite(v))
        {
            return Err(err("Nonfinite coupled thermal trajectory"));
        }
        let residual = max_of(&r.radiation_residual);
        if residual > 1e-8 * t.maximum_temperature_k.max(1.0) {
            return Err(err("Radiating surface temperature balance failed"));
        }
        if min_of(&r.minimum_temperature) < t.minimum_temperature_k
            || max_of(&r.maximum_temperature) > t.maximum_temperature_k
            || min_of(&r.minimum_capacity) <= 0.0
            || max_of(&r.maximum_outgoing) > 1.0
        {
            return Err(err(
                "Thermal positivity, validity or monotonicity limit exceeded; revise authored step/substeps",
            ));
        }
        let n = p.cells();
        let dv = p.spacing_m.powi(3);
        let energy: Vec<f64> = traj
            .temperature
            .iter()
            .zip(&traj.capacity)
            .map(|(tt, cc)| (0..n).map(|x| tt[x] * cc[x]).sum())
            .collect();
        let source = t.source_w_m3.iter().sum::<f64>() * dv + t.heat_flux_power_w.iter().sum::<f64>();
        let mut cumulative = 0.0;
        let mut error: f64 = 0.0;
        for (i, e) in energy.iter().enumerate() {
            if i > 0 {
                cumulative +=
                    r.boundary_energy[i - 1] + r.convection_energy[i - 1] + r.radiation_energy[i - 1];
            }
            let expected = energy[0] + i as f64 * p.step_s * source + cumulative;
            error = error.max((e - expected).abs());
        }
        let error = error / energy[0].abs().max(1e-30);
        if error > 1e-6 {
            return Err(err("Coupled thermal energy balance failed"));
        }
        let cp = t.fluid_specific_heat;
        let mut capacity_error: f64 = 0.0;
        for (f, c) in traj.f.iter().zip(&traj.capacity) {
            for x in 0..n {
                let expected = if p.solid_mask[x] {
                    t.solid_capacity[x]
                } else {
                    f[x * Q..(x + 1) * Q].iter().sum::<f64>() * p.density_kg_m3 * cp
                } * dv;
                capacity_error = capacity_error.max((c[x] - expected).abs() / expected);
            }
        }
        if capacity_error > 1e-6 {
            return Err(err("Thermal storage and LBM mass histories disagree"));
        }
        let mut tau_min = f64::INFINITY;
        let mut tau_max = f64::NEG_INFINITY;
        for temperature in &traj.temperature[..p.step_count] {
            for v in self.relaxation(temperature) {
                tau_min = tau_min.min(v);
                tau_max = tau_max.max(v);
            }
        }
        let reference = d.shift_remove("relaxation_time").unwrap_or(Value::Null);
        d.insert("reference_relaxation_time".into(), reference);
        d.insert("minimum_relaxation_time".into(), json!(tau_min));
        d.insert("maximum_relaxation_time".into(), json!(tau_max));
        let (beta, tref) = t.viscosity_feedback;
        let power = t.heat_flux_power_w.iter().sum::<f64>();
        d.insert("relative_thermal_energy_error".into(), json!(error));
        d.insert("maximum_thermal_outgoing_fraction".into(), json!(max_of(&r.maximum_outgoing)));
        d.insert("relative_mass_capacity_error".into(), json!(capacity_error));
        d.insert("boundary_sensible_energy_exchange_J".into(), json!(r.boundary_energy));
        d.insert("wall_convection_energy_exchange_J".into(), json!(r.convection_energy));
        d.insert("wall_radiation_energy_exchange_J".into(), json!(r.radiation_energy));
        d.insert("maximum_radiation_surface_residual_K".into(), json!(residual));
        d.insert("prescribed_wall_heat_power_W".into(), json!(power));
        d.insert("prescribed_wall_heat_energy_J".into(), json!(power * p.step_count as f64 * p.step_s));
        d.insert(
            "derivative_scope".into(),
            json!("Fixed-horizon LBM, conductivity, mass-consistent heat transport and authored staggered viscosity feedback"),
        );
        d.insert("thermal_feedback".into(), json!(beta != 0.0));
        d.insert("viscosity_law".into(), json!({"coefficient_per_K": beta, "reference_temperature_K": tref}));
        d.insert("coupling_schedule".into(), json!("temperature_at_interval_start"));
        d.insert("method_version".into(), json!("d3q19-thermal-staggered-viscosity-v1"));
        Ok(d)
    }


    pub fn solve(problem: &Value, design: &Design) -> CaeResult<ThermalSolved> {
        let tp = Self::normalize(problem)?;
        let raw = single_coordinate(design, COORDINATE, "Thermal LBM requires exactly model:control")?;
        let raw = tp.p.checked_design(raw)?;
        let model = tp.model(&raw);
        let trajectory = tp.trajectory(&model);
        let mut diagnostics = tp.admit(&model, &trajectory)?;
        if let Some(spec) = problem.get("environmental_ageing").filter(|v| !v.is_null()) {
            let p = &tp.p;
            let (positions, indices) = solid_indices(&p.solid_mask);
            let times = step_times(trajectory.temperature.len() - 1, p.step_s);
            let cells: Vec<Vec<f64>> =
                trajectory.temperature.iter().map(|t| positions.iter().map(|&i| t[i]).collect()).collect();
            let rows: Vec<&[f64]> = cells.iter().map(Vec::as_slice).collect();
            let mut observation = ageing::observe(spec, p.shape(), &times, Some(&indices), &rows)?;
            if let Some(m) = observation.as_object_mut() {
                m.insert("native_cell_indices".into(), json!(indices));
                m.insert("registration".into(), geometry::registration(p.shape(), p.spacing_m, p.origin_m)?);
            }
            diagnostics.insert("environmental_ageing".into(), observation);
        }
        Ok(ThermalSolved { tp, raw, model, trajectory, diagnostics })
    }

    #[must_use]
    pub fn vector(&self, model: &ThermalModel, traj: &Trajectory) -> [f64; 9] {
        let steps = self.p.step_count;
        let flow = self.p.vector(&model.phi, &model.alpha, &traj.f[steps]);
        let temperature = &traj.temperature[steps];
        let capacity = &traj.capacity[steps];
        let n = temperature.len() as f64;
        let mean = temperature.iter().sum::<f64>() / n;
        let var = temperature.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n;
        let stored: f64 = temperature.iter().zip(capacity).map(|(a, b)| a * b).sum();
        let mut out = [0.0; 9];
        out[..VECTOR_LEN].copy_from_slice(&flow);
        out[6] = mean;
        out[7] = var;
        out[8] = stored;
        out
    }
}

#[derive(Clone, Debug, Default)]
pub struct HistoryCotangents {
    pub post: Vec<Vec<f64>>,
    pub temperature: Vec<Vec<f64>>,
}

struct SubstepBar {
    temperature: Vec<f64>,
    capacity: Vec<f64>,
    conductivity: Vec<f64>,
    rates: Vec<Vec<f64>>,
    exchange: Vec<f64>,
}

#[allow(clippy::needless_pass_by_value)]
fn ad(e: AdError) -> CaeError {
    CaeError::contract(format!("thermal adjoint failed: {e}"))
}

impl ThermalProblem {
    fn substep_vjp(
        &self,
        m: &ThermalModel,
        drive: &FlowDrive,
        temperature: &[f64],
        capacity: &[f64],
        t_bar: &[f64],
        c_bar: &[f64],
    ) -> Result<SubstepBar, AdError> {
        let p = &self.p;
        let n = p.cells();
        let dx = p.spacing_m;
        let grid: Grid = p.grid;
        let mut tape = Tape::new();
        let tv = tape.input(temperature.to_vec());
        let cv = tape.input(capacity.to_vec());
        let kv = tape.input(m.k.clone());
        let rates: Vec<Var> = drive.rates.iter().map(|r| tape.input(r.clone())).collect();
        let ev = tape.input(drive.exchange.clone());

        let half = tape.map(kv, move |v| <Dual<1> as implexity_ad::Scalar>::from_f64(0.5 * dx) / v)?;
        let mut g = Vec::with_capacity(3);
        for axis in 0..3 {
            let other = tape.gather(half, m.rolls.forward[axis].clone())?;
            let r = tape.constant(self.t.contact_resistance.iter().map(|c| c[axis]).collect());
            let den = tape.add(half, r)?;
            let den = tape.add(den, other)?;
            let mask = crate::thermal::face_mask(&grid, p.periodic_axes, axis);
            let numer = tape.constant(mask.iter().map(|v| if *v { 1.0 } else { 0.0 } * (dx * dx)).collect());
            g.push(tape.div(numer, den)?);
        }

        let mut wall_g = tape.constant(vec![0.0; n]);
        let mut wall_gt = tape.constant(vec![0.0; n]);
        for patch in &self.t.convection_patches {
            let h = patch.coefficient_w_m2k;
            if h == 0.0 {
                continue;
            }
            let numer =
                tape.constant(patch.mask.iter().map(|v| if *v { 1.0 } else { 0.0 } * (dx * dx)).collect());
            let numer = tape.mul(numer, kv)?;
            let den = tape.map(kv, move |v| v / h + 0.5 * dx)?;
            let gp = tape.div(numer, den)?;
            wall_g = tape.add(wall_g, gp)?;
            let gpt = tape.scale(gp, patch.ambient_k)?;
            wall_gt = tape.add(wall_gt, gpt)?;
        }

        for patch in &self.t.radiation_patches {
            let s = Surface {
                spacing_m: dx,
                ambient_k: patch.ambient_k,
                emissivity: patch.emissivity,
                convection_w_m2k: patch.coefficient_w_m2k,
            };
            let mut gval = vec![0.0; n];
            let mut gtval = vec![0.0; n];
            let mut d = [vec![0.0; n], vec![0.0; n], vec![0.0; n], vec![0.0; n]];
            for x in 0..n {
                if !patch.mask[x] {
                    continue;
                }
                let e = surface_exchange(
                    Dual::<2>::new(temperature[x], [1.0, 0.0]),
                    Dual::new(m.k[x], [0.0, 1.0]),
                    &s,
                );
                gval[x] = e.conductance_w_k.re;
                gtval[x] = e.reservoir_power_w.re;
                d[0][x] = e.conductance_w_k.eps[0];
                d[1][x] = e.conductance_w_k.eps[1];
                d[2][x] = e.reservoir_power_w.eps[0];
                d[3][x] = e.reservoir_power_w.eps[1];
            }
            let [d0, d1, d2, d3] = d;
            let gnode = tape.custom(
                &[tv, kv],
                gval,
                Box::new(move |bar: &[f64]| {
                    Ok(vec![
                        bar.iter().zip(&d0).map(|(b, v)| b * v).collect(),
                        bar.iter().zip(&d1).map(|(b, v)| b * v).collect(),
                    ])
                }),
            )?;
            let gtnode = tape.custom(
                &[tv, kv],
                gtval,
                Box::new(move |bar: &[f64]| {
                    Ok(vec![
                        bar.iter().zip(&d2).map(|(b, v)| b * v).collect(),
                        bar.iter().zip(&d3).map(|(b, v)| b * v).collect(),
                    ])
                }),
            )?;
            wall_g = tape.add(wall_g, gnode)?;
            wall_gt = tape.add(wall_gt, gtnode)?;
        }
        let inputs = self.inputs(m, &drive.exchange);
        let step = energy_step_tape(
            &mut tape,
            &inputs,
            &m.rolls,
            tv,
            cv,
            [g[0], g[1], g[2]],
            &rates,
            ev,
            wall_g,
            wall_gt,
        )?;
        let out = tape.concat(&[step.temperature, step.capacity])?;
        let mut cot = t_bar.to_vec();
        cot.extend_from_slice(c_bar);
        let grads = tape.vjp(out, &cot)?;
        Ok(SubstepBar {
            temperature: grads.wrt(tv)?,
            capacity: grads.wrt(cv)?,
            conductivity: grads.wrt(kv)?,
            rates: rates.iter().map(|r| grads.wrt(*r)).collect::<Result<_, _>>()?,
            exchange: grads.wrt(ev)?,
        })
    }


    pub fn adjoint(
        &self,
        raw: &[f64],
        m: &ThermalModel,
        traj: &Trajectory,
        f_bar: Vec<f64>,
        t_bar: Vec<f64>,
        c_bar: Vec<f64>,
        alpha_bar: Vec<f64>,
        phi_bar: Vec<f64>,
    ) -> CaeResult<Vec<f64>> {
        self.adjoint_history(
            raw,
            m,
            traj,
            f_bar,
            t_bar,
            c_bar,
            alpha_bar,
            phi_bar,
            &HistoryCotangents::default(),
        )
    }


    pub fn adjoint_history(
        &self,
        raw: &[f64],
        m: &ThermalModel,
        traj: &Trajectory,
        mut f_bar: Vec<f64>,
        mut t_bar: Vec<f64>,
        mut c_bar: Vec<f64>,
        mut alpha_bar: Vec<f64>,
        mut phi_bar: Vec<f64>,
        extra: &HistoryCotangents,
    ) -> CaeResult<Vec<f64>> {
        let p = &self.p;
        let n = p.cells();
        let mut k_bar = vec![0.0; n];
        let scale = p.density_kg_m3 * p.spacing_m.powi(3) / p.step_s;
        if let Some(tb) = extra.temperature.get(p.step_count).filter(|v| !v.is_empty()) {
            for (a, b) in t_bar.iter_mut().zip(tb) {
                *a += b;
            }
        }
        for step in (0..p.step_count).rev() {
            let before = &traj.f[step];
            let temperature = &traj.temperature[step];
            let drive = self.drive(m, before, temperature);
            let mut temps = vec![temperature.clone()];
            let mut caps = vec![traj.capacity[step].clone()];
            for s in 0..self.t.substeps {
                let (t1, c1, _, _) = self.substep(m, &drive, &temps[s], &caps[s]);
                temps.push(t1);
                caps.push(c1);
            }
            let mut rates_bar = vec![vec![0.0; n]; POSITIVE.len()];
            let mut exchange_bar = vec![0.0; n];
            for s in (0..self.t.substeps).rev() {
                let b = self.substep_vjp(m, &drive, &temps[s], &caps[s], &t_bar, &c_bar).map_err(ad)?;
                t_bar = b.temperature;
                c_bar = b.capacity;
                for x in 0..n {
                    k_bar[x] += b.conductivity[x];
                    exchange_bar[x] += b.exchange[x];
                }
                for (acc, r) in rates_bar.iter_mut().zip(&b.rates) {
                    for x in 0..n {
                        acc[x] += r[x];
                    }
                }
            }
            let mut post_extra = vec![0.0; n * Q];
            mass_rates_from_post_vjp(p, &rates_bar, &mut post_extra);
            if let Some(pb) = extra.post.get(step).filter(|v| !v.is_empty()) {
                for (a, b) in post_extra.iter_mut().zip(pb) {
                    *a += b;
                }
            }
            let port_exchange_bar: Vec<f64> =
                p.ports.iter().flat_map(|q| q.port.cells.iter().map(|&x| exchange_bar[x] * scale)).collect();
            let tau = self.relaxation(temperature);
            let (fb, ab, taub) = p.interval_vjp(
                before,
                &m.alpha,
                Tau::PerCell(&tau),
                &f_bar,
                Some(&post_extra),
                Some(&port_exchange_bar),
            )?;
            let dtau = self.relaxation_derivative(temperature);
            for x in 0..n {
                t_bar[x] += taub[x] * dtau[x];
                alpha_bar[x] += ab[x];
            }
            if let Some(tb) = extra.temperature.get(step).filter(|v| !v.is_empty()) {
                for (a, b) in t_bar.iter_mut().zip(tb) {
                    *a += b;
                }
            }
            f_bar = fb;
        }
        let dres = p.resistance_derivative(&m.phi);
        for x in 0..n {
            phi_bar[x] += alpha_bar[x] * dres[x]
                + k_bar[x] * (self.t.fluid_conductivity - self.t.solid_conductivity[x]);
        }
        Ok(p.design_map.vjp(raw, &phi_bar))
    }
}

#[must_use]
pub fn editor() -> Value {
    let flow = provider::problem_template();
    let shape = provider::TEMPLATE_SHAPE;
    let n: usize = shape.iter().product();
    let template = json!({
        "flow": flow,
        "environmental_ageing": null,
        "thermal": {
            "initial_temperature_K": to_nested(&vec![300.0; n], &shape),
            "fluid_specific_heat_J_kgK": 4200.0, "solid_capacity_J_m3K": to_nested(&vec![3e6; n], &shape),
            "fluid_conductivity_W_mK": 0.6, "solid_conductivity_W_mK": 15.0,
            "source_W_m3": to_nested(&vec![0.0; n], &shape), "minimum_temperature_K": 273.0, "maximum_temperature_K": 373.0,
            "substeps": 8, "history_byte_budget": 16_000_000,
            "port_temperatures_K": {},
            "heat_flux_patches": [],
            "convection_patches": [],
            "radiation_patches": [],
            "viscosity_feedback": {"coefficient_per_K": 0.0, "reference_temperature_K": 300.0},
            "provenance": "Synthetic demonstration; supply calibrated material data",
        },
    });
    json!({
        "kind": "native_json",
        "title": "Lattice Boltzmann + heat transport \u{2014} experimental",
        "design_template": provider::design_template(&shape),
        "problem_template": template,
        "schema": {"type": "object", "properties": {
            "flow": {"title": "LBM flow and topology map", "format": "json"},
            "environmental_ageing": {"title": "Optional solid-temperature ageing observer", "format": "json",
                "description": "null or settings, two materials (density kg/m\u{b3}, T_min/T_max K), full-grid fixed composition [0,1] and history_byte_budget. Uses fixed solid_mask cells in C-order only. Law activities shape [step_count+1,number of solid cells,2]. Empty solid masks are rejected. Actual physical seconds; diagnostics report native_cell_indices, extents, chemical storage J/m\u{b3} and interval power W/m\u{b3}. No thermal/property feedback or ageing objective."},
            "thermal": {"title": "Thermal materials, source, validity and substeps", "format": "json",
                "description": "Unspecified closed faces are insulated. heat_flux_patches specify id, face, mask and inward_flux_W_m2 (positive heats domain). convection_patches specify id, face, mask, coefficient_W_m2K and ambient_temperature_K, including half-cell conduction resistance. Boundary patches must not overlap. port_temperatures_K maps every flow port id to reservoir temperature, including outlet backflow. viscosity_feedback defines nu(T)=nu_ref*exp(coefficient_per_K*(T-reference_temperature_K)); zero disables feedback."}}},
    })
}


pub(crate) fn thermal_fields(s: &ThermalSolved) -> CaeResult<BTreeMap<String, FieldValue>> {
    let p = &s.tp.p;
    let steps = p.step_count;
    let traj = &s.trajectory;
    let shape = p.shape().to_vec();
    let (rho, u) = p.fields(&s.model.alpha, &traj.f[steps]);
    let mut vshape = shape.clone();
    vshape.push(3);
    let mut hshape = vec![steps + 1];
    hshape.extend_from_slice(&shape);
    let tau = s.tp.relaxation(&traj.temperature[steps - 1]);
    let wall = wall_exchange::loads(p, &s.model.alpha, &traj.f[steps - 1], 0.0, Tau::PerCell(&tau));
    let mut fields = BTreeMap::new();
    fields.insert("fluid_fraction".into(), FieldValue::Array(array(s.model.phi.clone(), &shape)?));
    fields.insert("density_kg_m3".into(), FieldValue::Array(array(rho, &shape)?));
    fields.insert("velocity_m_s".into(), FieldValue::Array(array(u, &vshape)?));
    fields.insert("temperature_K".into(), FieldValue::Array(array(traj.temperature[steps].clone(), &shape)?));
    fields.insert(
        "temperature_history_K".into(),
        FieldValue::Array(array(traj.temperature.iter().flatten().copied().collect(), &hshape)?),
    );
    fields.insert(
        "time_s".into(),
        FieldValue::Array(array((0..=steps).map(|i| i as f64 * p.step_s).collect(), &[steps + 1])?),
    );
    insert_wall_fields(&mut fields, &wall, "wall_link_gauge_force_N")?;
    Ok(fields)
}

pub(crate) fn response_cotangents(
    s: &ThermalSolved,
    index: usize,
) -> (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) {
    let p = &s.tp.p;
    let steps = p.step_count;
    let n = p.cells();
    let traj = &s.trajectory;
    if index < VECTOR_LEN {
        let mut w = [0.0; VECTOR_LEN];
        w[index] = 1.0;
        let (f, a, phi) = p.vector_vjp(&s.model.phi, &s.model.alpha, &traj.f[steps], &w);
        return (f, vec![0.0; n], vec![0.0; n], a, phi);
    }
    let temperature = &traj.temperature[steps];
    let nn = n as f64;
    let (t_bar, c_bar) = match index {
        6 => (vec![1.0 / nn; n], vec![0.0; n]),
        7 => {
            let mean = temperature.iter().sum::<f64>() / nn;
            (temperature.iter().map(|v| 2.0 * (v - mean) / nn).collect(), vec![0.0; n])
        }
        _ => (traj.capacity[steps].clone(), temperature.clone()),
    };
    (vec![0.0; n * Q], t_bar, c_bar, vec![0.0; n], vec![0.0; n])
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ThermalLbmProvider;

impl ThermalLbmProvider {
    pub const IMPLEMENTATION: &'static str = "implexity.lbm.thermal_provider.ThermalLBMProvider";


    pub fn declaration(problem: &Value) -> CaeResult<CouplingDeclaration> {
        let tp = ThermalProblem::normalize(problem)?;
        let edge = |s: &str, t: &str, q: &str, reason: &str| {
            CouplingEdge::new(s, t, q, "one_way", true, reason).map_err(|e| err(e.0))
        };
        let mut edges = vec![edge(
            "flow",
            "thermal",
            "population_mass_flux_history",
            "Actual mass flux transports sensible energy",
        )?];
        if tp.t.viscosity_feedback.0 != 0.0 {
            edges.push(edge(
                "thermal",
                "flow",
                "temperature_dependent_viscosity",
                "Previous interval temperature controls next collision relaxation",
            )?);
        }
        let loops =
            if edges.len() == 2 { vec![vec!["flow".to_string(), "thermal".to_string()]] } else { Vec::new() };
        let mut physics = vec!["flow".to_string(), "thermal".to_string()];
        if problem.get("environmental_ageing").is_some_and(|v| !v.is_null()) {
            physics.push("environmental_ageing".into());
            edges.push(edge(
                "thermal",
                "environmental_ageing",
                "fixed_solid_temperature_history",
                "Calibrated physical-time observer on fixed solid cells only; no heat/property feedback",
            )?);
        }
        Ok(CouplingDeclaration {
            provider: NAME.into(),
            active_physics: physics,
            edges,
            closed_loops: loops,
            notes: NOTES.iter().map(|s| (*s).to_string()).collect(),
            ..CouplingDeclaration::default()
        })
    }
}

impl LbmOperations for ThermalLbmProvider {
    fn preflight_value(&self, problem: &Value, design: &Design) -> CaeResult<Map<String, Value>> {
        let s = ThermalProblem::solve(problem, design)?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("issues".into(), json!([]));
        out.extend(s.diagnostics);
        Ok(out)
    }

    fn evaluate_value(&self, problem: &Value, design: &Design) -> CaeResult<Evaluation> {
        let s = ThermalProblem::solve(problem, design)?;
        let values = s.tp.vector(&s.model, &s.trajectory);
        let fields = thermal_fields(&s)?;
        Ok(Evaluation {
            provider: NAME.into(),
            responses: RESPONSES.iter().zip(values).map(|(n, v)| ((*n).to_string(), v)).collect(),
            diagnostics: s.diagnostics,
            fields,
        })
    }

    fn sensitivities_value(
        &self,
        problem: &Value,
        design: &Design,
        responses: &[String],
    ) -> CaeResult<DesignSensitivities> {
        check_responses(responses, &RESPONSES, "Unknown, empty or duplicated thermal LBM responses")?;
        let s = ThermalProblem::solve(problem, design)?;
        let values = s.tp.vector(&s.model, &s.trajectory);
        let mut out_values = BTreeMap::new();
        let mut gradients = BTreeMap::new();
        for name in responses {
            let index = RESPONSES.iter().position(|r| r == name).unwrap_or(0);
            let (f, t, c, a, phi) = response_cotangents(&s, index);
            let gradient = s.tp.adjoint(&s.raw, &s.model, &s.trajectory, f, t, c, a, phi)?;
            if !gradient.iter().all(|v| v.is_finite()) {
                return Err(err("Nonfinite coupled thermal sensitivity"));
            }
            out_values.insert(name.clone(), values[index]);
            let mut g = Design::new();
            g.insert(COORDINATE, array(gradient, &s.tp.p.shape())?);
            gradients.insert(name.clone(), g);
        }
        Ok(DesignSensitivities { responses: out_values, gradients, diagnostics: s.diagnostics })
    }

    fn admission_value(&self, problem: &Value, candidate: &Design) -> CaeResult<Map<String, Value>> {
        Ok(ThermalProblem::solve(problem, candidate)?.diagnostics)
    }
}

crate::lbm_design_operations!(ThermalLbmProvider, COORDINATE);

impl CaeProvider for ThermalLbmProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn implementation(&self) -> &str {
        Self::IMPLEMENTATION
    }

    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        let d = descriptor(
            NAME,
            &["flow_thermal"],
            &RESPONSES,
            &UNITS,
            &[
                "fluid_fraction",
                "density_kg_m3",
                "velocity_m_s",
                "temperature_K",
                "temperature_history_K",
                "time_s",
                "wall_link_positions_m",
                "wall_link_gauge_force_N",
            ],
            &NOTES,
        );
        presentation(&d, editor(), "array")
    }

    fn orchestration_contract(&self) -> Option<CaeResult<PublishedContract>> {
        let notes: Vec<String> = NOTES.iter().map(|s| (*s).to_string()).collect();
        Some(
            contract(NAME, &RESPONSES, &UNITS, &["flow", "thermal"], &notes)
                .map(|c| PublishedContract::Contract(Box::new(c))),
        )
    }

    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        let tp = ThermalProblem::normalize(problem)?;
        let map = problem.as_object().cloned().unwrap_or_default();
        Ok(Arc::new(geometry::registered_problem(&map, tp.p.shape(), tp.p.spacing_m, tp.p.origin_m)?))
    }

    fn preflight(
        &self,
        problem: &ProviderProblem,
        _topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        ThermalProblem::normalize(problem_value(problem)?)?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("issues".into(), json!([]));
        out.insert("requires_complete_design".into(), json!(true));
        out.insert("physical_qualification".into(), json!(false));
        Ok(out)
    }

    fn evaluate(&self, _problem: &ProviderProblem, _topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        Err(missing_method("ThermalLBMProvider", "evaluate"))
    }

    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        _topology: &ArrayD<f64>,
        _response: &str,
    ) -> CaeResult<Sensitivity> {
        Err(missing_method("ThermalLBMProvider", "sensitivity"))
    }

    fn coupling_declaration(&self, problem: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        let problem = match problem.map(problem_value) {
            Some(Ok(v)) => v,
            Some(Err(e)) => return Some(Err(e)),
            None => return Some(Err(err("Thermal LBM coupling declaration requires a problem"))),
        };
        Some(Self::declaration(problem).map(|d| d.to_value()))
    }

    fn interface(&self, name: &str) -> Option<&(dyn Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(name)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub const BASE: LatticeBoltzmannProvider = LatticeBoltzmannProvider;
