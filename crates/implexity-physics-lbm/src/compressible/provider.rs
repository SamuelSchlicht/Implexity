// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, ProviderCapabilities, ProviderProblem, Sensitivity,
};
use implexity_core::coupling_graph::{CouplingDeclaration, CouplingEdge};
use implexity_core::orchestration::PublishedContract;
use implexity_core::{CaeError, CaeResult};
use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use super::equilibrium::Lattice;
use super::history::{
    Cotangents, GAS_RESPONSES, History, Model, PORT_RESPONSES, SolidModel, Units, fields, gas_responses,
    gas_responses_vjp, initial_state, port_exchange_bar, port_responses,
};
use super::solid::{ViscosityLaw, conduction_map, reservoir_map, wall_flux_map};
use super::step::{StageBar, State};
use super::structure::{self, GridInfo, PorousStructure, StructureHistory};
use super::transport::{Transport, box_transport_map, equilibrium_reservoir_boundary, slab_map};
use crate::ageing;
use crate::d3q19::Grid;
use crate::design::{DesignMap, TopologyMap, resistance, resistance_derivative};
use crate::design_ops::{Design, DesignSensitivities, LbmOperations, array, presentation, problem_value};
use crate::geometry;
use crate::nparray::{NdArray, asarray, py_int, to_nested, to_nested_bool};
use crate::provider::{COORDINATE, contract, descriptor, missing_method};
use crate::solver::periodic_of;
use implexity_physics_solid::fatigue::{evaluate_rainflow, validate_rainflow_settings};

pub const NAME: &str = "lattice_boltzmann_compressible_d3q343";
pub const SOLID_RESPONSES: [&str; 2] = ["mean_solid_temperature_K", "solid_stored_energy_change_J"];
pub const STRUCTURE_RESPONSES: [&str; 1] = ["mean_squared_porous_stress_Pa2"];
pub const SOLID_RESERVOIR_RESPONSES: [&str; 3] =
    ["solid_convection_energy_J", "solid_radiation_energy_J", "solid_environment_energy_J"];

#[must_use]
pub fn response_names() -> Vec<&'static str> {
    GAS_RESPONSES
        .iter()
        .chain(&SOLID_RESPONSES)
        .chain(&STRUCTURE_RESPONSES)
        .chain(&PORT_RESPONSES)
        .chain(&SOLID_RESERVOIR_RESPONSES)
        .copied()
        .collect()
}

fn unit_of(name: &str) -> &'static str {
    match name {
        "gas_mass_kg" => "kg",
        "kinetic_energy_J" | "internal_energy_J" | "solid_stored_energy_change_J" => "J",
        n if n.starts_with("mean_velocity") => "m/s",
        "mean_temperature_K" | "mean_solid_temperature_K" => "K",
        "mean_pressure_Pa" => "Pa",
        "porous_fluid_volume_m3" => "m^3",
        "mean_squared_porous_stress_Pa2" => "Pa^2",
        n if n.ends_with("mass_flow_kg_s") => "kg/s",
        n if n.ends_with("energy_W") => "W",
        n if n.ends_with("_N") => "N",
        _ => "J",
    }
}

const PRESENTATION: &str = include_str!("presentation.json");

#[must_use]
pub fn problem_template() -> Value {
    let shape = [3usize, 3, 3];
    json!({
        "shape": shape, "origin_m": [0.0, 0.0, 0.0], "spacing_m": 0.01, "step_s": 0.00002, "step_count": 2,
        "density_unit_kg_m3": 1.0, "gas_constant_J_kg_K": 287.0, "gamma": 1.4, "tau": 0.6,
        "initial_density_kg_m3": 1.0, "initial_velocity_m_s": [50.0, 0.0, 0.0], "initial_fields": null,
        "wall_heat_flux_patches": [], "environmental_ageing": null,
        "initial_temperature_K": 522.648_083_623_693_4, "periodic_axes": [true, true, true], "reservoirs": null,
        "solid_thermal": null, "viscosity": null, "porous_structure": null, "fatigue": null, "slab_bounceback_axis": null,
        "design_region": to_nested_bool(&[true; 27], &shape), "fixed_design": to_nested(&[1.0; 27], &shape),
        "geometry_source": null,
        "drag_max_per_s": 100.0, "drag_shape": 0.1, "history_byte_budget": 64_000_000,
        "topology_map": {"filter_radius_m": 0.015, "projection_beta": 2.0, "projection_eta": 0.5},
    })
}

fn presentation_data() -> Value {
    serde_json::from_str(PRESENTATION).unwrap_or(Value::Null)
}

fn notes() -> Vec<String> {
    presentation_data()["notes"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Debug)]
pub struct DesignProblem {
    pub grid: Grid,
    pub spacing_m: f64,
    pub step_s: f64,
    pub periodic: [bool; 3],
    pub map: DesignMap,
    pub drag_max_per_s: f64,
    pub drag_shape: f64,
    pub origin_m: [f64; 3],
    pub registration: Value,
    pub geometry_sampling: Option<Value>,
}

impl DesignProblem {
    #[must_use]
    pub fn fields(&self, raw: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let phi = self.map.map(raw);
        let exposure =
            phi.iter().map(|v| resistance(self.step_s, self.drag_max_per_s, self.drag_shape, *v)).collect();
        (phi, exposure)
    }


    pub fn checked_coordinate(&self, raw: &ArrayD<f64>) -> CaeResult<Vec<f64>> {
        if raw.shape() != self.grid.shape || raw.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
            return Err(err("Compressible design coordinates must match the grid and lie in [0,1]"));
        }
        Ok(raw.iter().copied().collect())
    }
}

#[derive(Clone, Debug)]
pub struct Prepared {
    pub design: DesignProblem,
    pub units: Units,
    pub model: Model,
    pub initial: State,
    pub problem: Map<String, Value>,
    pub structure: Option<PorousStructure>,
}

const OPTIONAL: [&str; 11] = [
    "reservoirs",
    "solid_thermal",
    "origin_m",
    "viscosity",
    "porous_structure",
    "fatigue",
    "slab_bounceback_axis",
    "geometry_source",
    "initial_fields",
    "wall_heat_flux_patches",
    "environmental_ageing",
];

fn present(p: &Map<String, Value>, key: &str) -> bool {
    p.get(key).is_some_and(|v| !v.is_null())
}

fn scalar_nonneg(p: &Map<String, Value>, key: &str, nonzero: bool) -> CaeResult<f64> {
    match asarray(p.get(key).unwrap_or(&Value::Null)).real_scalar() {
        Some(v) if v >= 0.0 && !(nonzero && v == 0.0) => Ok(v),
        _ => Err(err(format!("Invalid compressible design parameter: {key}"))),
    }
}


#[allow(clippy::too_many_lines)]
pub fn prepare(problem: &Value) -> CaeResult<Prepared> {
    let template = problem_template();
    let required: Vec<&str> = template
        .as_object()
        .map(|m| m.keys().map(String::as_str).filter(|k| !OPTIONAL.contains(k)).collect())
        .unwrap_or_default();
    let ok = problem.as_object().is_some_and(|m| {
        required.iter().all(|k| m.contains_key(*k))
            && m.keys().all(|k| required.contains(&k.as_str()) || OPTIONAL.contains(&k.as_str()))
    });
    let Some(p) = problem.as_object().filter(|_| ok) else {
        return Err(err("Compressible LBM requires exactly the documented problem fields"));
    };
    if present(p, "fatigue") {
        if !present(p, "porous_structure") {
            return Err(err("Fatigue requires porous_structure stress history"));
        }
        let settings = validate_rainflow_settings(&p["fatigue"])?;
        let solved = settings.get("temperature_source").and_then(Value::as_str) == Some("solved_solid");
        let mapped = p["porous_structure"].as_object().is_some_and(|m| m.contains_key("thermal_expansion"));
        if solved && !mapped {
            return Err(err("Solved-solid fatigue requires the explicit thermal-expansion temperature map"));
        }
    }
    let steps = py_int(&p["step_count"]).filter(|v| *v >= 1);
    let budget = py_int(&p["history_byte_budget"]).filter(|v| *v >= 1);
    let (Some(steps), Some(budget)) = (steps, budget) else {
        return Err(err("Positive integer step count and history budget required"));
    };
    let steps = usize::try_from(steps).unwrap_or(1);
    let geometry_source = present(p, "geometry_source");
    let shape_list = p["shape"].as_array().filter(|a| a.len() == 3);
    let shape: Option<[usize; 3]> = shape_list.and_then(|a| {
        let v: Vec<usize> = a
            .iter()
            .filter_map(|x| py_int(x).filter(|n| *n >= 1).and_then(|n| usize::try_from(n).ok()))
            .collect();
        (v.len() == 3).then(|| [v[0], v[1], v[2]])
    });
    let (region_raw, fixed_raw) = if geometry_source {
        if !p["design_region"].is_null() || !p["fixed_design"].is_null() {
            return Err(err("geometry_source owns design_region/fixed_design: set both to null"));
        }
        let Some(shape) = shape else {
            return Err(err("Three positive integer grid dimensions required"));
        };
        let n: usize = shape.iter().product();
        (
            NdArray { shape: shape.to_vec(), kind: crate::nparray::Kind::Bool, data: vec![1.0; n] },
            NdArray { shape: shape.to_vec(), kind: crate::nparray::Kind::Float, data: vec![1.0; n] },
        )
    } else {
        (asarray(&p["design_region"]), asarray(&p["fixed_design"]))
    };

    let Some(shape) = shape else {
        return Err(err("Three positive integer grid dimensions required"));
    };
    let spacing = scalar_nonneg(p, "spacing_m", true)?;
    let step = scalar_nonneg(p, "step_s", true)?;
    let drag_max = scalar_nonneg(p, "drag_max_per_s", false)?;
    let drag_shape = scalar_nonneg(p, "drag_shape", true)?;
    let periodic = periodic_of(&p["periodic_axes"]);
    let Some(periodic) = periodic.filter(|_| region_raw.shape == shape && region_raw.is_bool()) else {
        return Err(err("Boolean grid design region and three periodic flags required"));
    };
    if fixed_raw.shape != shape
        || !fixed_raw.is_real()
        || !fixed_raw.all_finite()
        || fixed_raw.data.iter().any(|v| !(0.0..=1.0).contains(v))
    {
        return Err(err("Protected fluid fractions must match the grid and lie in [0,1]"));
    }
    let topology = TopologyMap::normalize(&p["topology_map"], spacing)?;
    let origin = geometry::origin(p.get("origin_m").unwrap_or(&json!([0.0, 0.0, 0.0])))?;
    let registration = geometry::registration(shape, spacing, origin)?;
    let grid = Grid::new(shape);
    let n = grid.cells();
    let mut region = region_raw.bools();
    let mut fixed = fixed_raw.data;
    let mut geometry_sampling = None;
    if geometry_source {
        let source = &p["geometry_source"];
        let keys = ["document", "porous_obstacle_nodes", "design_nodes", "protected_fluid_nodes"];
        let Some(s) = source.as_object().filter(|m| m.len() == 4 && keys.iter().all(|k| m.contains_key(*k)))
        else {
            return Err(err(
                "Compressible geometry requires document, porous_obstacle_nodes, design_nodes and protected_fluid_nodes",
            ));
        };
        let mapped = json!({
            "document": s["document"], "solid_nodes": s["porous_obstacle_nodes"],
            "design_nodes": s["design_nodes"], "protected_fluid_nodes": s["protected_fluid_nodes"],
        });
        let masks = geometry::masks_from_document(shape, spacing, origin, &mapped, true)?;
        region = masks.design;
        fixed = masks.fixed;
        let mut sampling = masks.sampling;
        if let Value::Object(m) = &mut sampling {
            m.insert(
                "obstacle_semantics".into(),
                json!("fixed finite porous resistance, not impermeable or bounce-back geometry"),
            );
        }
        geometry_sampling = Some(sampling);
    }
    let mut porous = None;
    if present(p, "porous_structure") {
        if p["porous_structure"].get("thermal_expansion").is_some_and(|v| !v.is_null())
            && !present(p, "solid_thermal")
        {
            return Err(err("Thermal expansion requires solid_thermal storage"));
        }
        let info =
            GridInfo { shape, origin_m: origin, spacing_m: spacing, step_s: step, drag_max_per_s: drag_max };
        porous = Some(structure::prepare(&p["porous_structure"], &info, &p["density_unit_kg_m3"], steps)?);
    }
    let solid_present = present(p, "solid_thermal");
    let history_bytes = (i128::try_from(steps).unwrap_or(0) + 1) * 343 * 2
        + if solid_present { 2 * steps as i128 } else { 0 };
    if history_bytes * n as i128 * 8 > i128::from(budget) {
        return Err(err(
            "Compressible population/solid-temperature history exceeds budget; AD needs additional memory",
        ));
    }
    let units = Units::new(
        spacing,
        step,
        p["density_unit_kg_m3"].as_f64().unwrap_or(f64::NAN),
        p["gas_constant_J_kg_K"].as_f64().unwrap_or(f64::NAN),
    )?;
    let mut solid_model = None;
    if solid_present {
        let solid = &p["solid_thermal"];
        let required = ["initial_temperature_K", "capacity_J_K", "conductance_W_K", "gas_heat_fraction"];
        let ok = solid.as_object().is_some_and(|m| {
            required.iter().all(|k| m.contains_key(*k))
                && m.keys()
                    .all(|k| required.contains(&k.as_str()) || k == "conduction" || k == "reservoir_patches")
        });
        let Some(solid) = solid.as_object().filter(|_| ok) else {
            return Err(err("Solid thermal storage requires the documented four fields"));
        };
        let mut values = BTreeMap::new();
        for key in ["initial_temperature_K", "capacity_J_K", "conductance_W_K"] {
            let a = asarray(&solid[key]);
            if a.shape != shape
                || !a.is_real()
                || !a.all_finite()
                || a.data.iter().any(|v| *v < 0.0 || (key != "conductance_W_K" && *v == 0.0))
            {
                return Err(err(format!("Invalid cellwise solid {key}")));
            }
            values.insert(key, a.data);
        }
        let fraction = asarray(&solid["gas_heat_fraction"]).real_scalar().filter(|v| (0.0..=1.0).contains(v));
        let Some(fraction) = fraction else {
            return Err(err("gas_heat_fraction must lie in [0,1]"));
        };
        let tu = units.temperature_unit_k;
        let eu = units.energy_unit_j;
        let capacity_phys = values.remove("capacity_J_K").unwrap_or_default();
        let mut model = SolidModel {
            temperature: values["initial_temperature_K"].iter().map(|v| v / tu).collect(),
            capacity: capacity_phys.iter().map(|v| v * tu / eu).collect(),
            conductance_dt: values["conductance_W_K"].iter().map(|v| v * step * tu / eu).collect(),
            gas_heat_fraction: fraction,
            conduction: None,
            reservoir: None,
        };
        let conduction = solid.get("conduction").filter(|v| !v.is_null());
        let mut conductivity: Option<Vec<f64>> = None;
        if let Some(c) = conduction {
            let Some(cm) = c.as_object().filter(|m| {
                m.len() == 2 && m.contains_key("conductivity_W_m_K") && m.contains_key("substeps")
            }) else {
                return Err(err("Solid conduction requires conductivity_W_m_K and substeps"));
            };
            let k = asarray(&cm["conductivity_W_m_K"]);
            if k.shape.len() != 3 || k.shape != shape || !k.is_real() {
                return Err(err("Finite nonnegative conductivity and positive cell capacity required"));
            }
            let substeps = py_int(&cm["substeps"]).filter(|s| *s >= 1).and_then(|s| usize::try_from(s).ok());
            let Some(substeps) = substeps else {
                return Err(err("Positive fixed conduction substep count required"));
            };
            let mut map = conduction_map(grid, &k.data, &capacity_phys, spacing, step, periodic, substeps)?;
            for l in &mut map.links_dt {
                for v in l.iter_mut() {
                    *v *= tu / eu;
                }
            }
            model.conduction = Some(map);
            conductivity = Some(k.data);
        }
        match solid.get("reservoir_patches") {
            Some(patches) if patches.as_array().is_some_and(|a| !a.is_empty()) => {
                let Some(k) = &conductivity else {
                    return Err(err(
                        "Solid reservoir patches require conduction conductivity for half-cell surface resistance",
                    ));
                };
                model.reservoir = Some(reservoir_map(grid, patches, k, spacing, step, periodic, tu, eu)?);
            }
            Some(patches) if !patches.is_array() && !patches.is_null() => {
                return Err(err("Solid reservoir_patches must be a list"));
            }
            _ => {}
        }
        solid_model = Some(model);
    }
    let gamma = asarray(&p["gamma"]).real_scalar();
    let tau = asarray(&p["tau"]).real_scalar();
    let (Some(gamma), Some(tau)) = (gamma, tau) else {
        let key = if gamma.is_none() { "gamma" } else { "tau" };
        return Err(err(format!("Finite scalar {key} required")));
    };
    if !(0.5 < tau && tau <= 1.0 / 1.35) {
        return Err(err("Require 0.5 < tau <= 1/1.35"));
    }
    let lattice = Lattice::D3q343WeightedM26;
    let (initial, initial_temperature) = initial_state(p, shape, &units, gamma, lattice)?;
    let mut transport: Transport = box_transport_map(shape, periodic, lattice)?;
    let slab_axis = p.get("slab_bounceback_axis").filter(|v| !v.is_null());
    if let Some(axis) = slab_axis {
        let axis = py_int(axis).filter(|a| (0..3).contains(a)).and_then(|a| usize::try_from(a).ok());
        let Some(axis) = axis.filter(|a| (0..3).all(|b| periodic[b] == (b != *a))) else {
            return Err(err("Slab axis requires one nonperiodic normal and two periodic transverse axes"));
        };
        if present(p, "reservoirs") {
            return Err(err("Slab bounce-back is incompatible with reservoir faces"));
        }
        transport = slab_map(shape, axis, lattice)?;
    }
    let reservoirs = present(p, "reservoirs");
    if reservoirs {
        let r = &p["reservoirs"];
        let Some(rm) = r.as_object().filter(|m| {
            m.len() == 3
                && ["density_kg_m3", "velocity_m_s", "temperature_K"].iter().all(|k| m.contains_key(*k))
        }) else {
            return Err(err("Reservoirs require density_kg_m3, velocity_m_s and temperature_K"));
        };
        if periodic != [false, true, true] {
            return Err(err("Open x reservoirs require periodic y/z and nonperiodic x"));
        }
        let mut arrays = BTreeMap::new();
        for (key, expected) in
            [("density_kg_m3", vec![2]), ("velocity_m_s", vec![2, 3]), ("temperature_K", vec![2])]
        {
            let a = asarray(&rm[key]);
            if a.shape != expected || !a.is_real() || !a.all_finite() {
                return Err(err(format!("Invalid reservoir {key}")));
            }
            arrays.insert(key, a.data);
        }
        let d = &arrays["density_kg_m3"];
        let v = &arrays["velocity_m_s"];
        let t = &arrays["temperature_K"];
        let vu = units.velocity_unit_m_s;
        let du = units.density_unit_kg_m3;
        let tu = units.temperature_unit_k;
        transport = equilibrium_reservoir_boundary(
            shape,
            [d[0] / du, d[1] / du],
            [[v[0] / vu, v[1] / vu, v[2] / vu], [v[3] / vu, v[4] / vu, v[5] / vu]],
            [t[0] / tu, t[1] / tu],
            gamma,
            lattice,
        )?;
    }
    let wall_heat = wall_flux_map(
        p.get("wall_heat_flux_patches").unwrap_or(&json!([])),
        grid,
        periodic,
        spacing,
        step,
        units.energy_unit_j,
        reservoirs,
    )?;
    let mut viscosity = None;
    if present(p, "viscosity") {
        let law = ViscosityLaw::normalize(&p["viscosity"], spacing, step, units.temperature_unit_k)?;
        let lo = initial_temperature.iter().fold(f64::INFINITY, |a, b| a.min(*b));
        let hi = initial_temperature.iter().fold(f64::NEG_INFINITY, |a, b| a.max(*b));
        law.admit_temperature(lo, hi)?;
        viscosity = Some(law);
    }
    if let Some(ps) = porous.as_mut() {
        let info =
            GridInfo { shape, origin_m: origin, spacing_m: spacing, step_s: step, drag_max_per_s: drag_max };
        structure::attach_wall_loads(ps, &info, &mut transport, &p["density_unit_kg_m3"], steps)?;
    }
    if present(p, "environmental_ageing") {
        if solid_model.is_none() {
            return Err(err(
                "Environmental ageing requires solved solid_thermal; gas temperature is not substituted",
            ));
        }
        if let Some(s) = &solid_model {
            let times = step_times(steps, step);
            let initial: Vec<f64> = s.temperature.iter().map(|v| v * units.temperature_unit_k).collect();
            ageing::admit_initial(&p["environmental_ageing"], grid.shape, &times, None, &initial)?;
        }
    }
    let map = DesignMap::new(grid, periodic, &vec![false; n], &region, &fixed, spacing, &topology);
    let mut initial = initial;
    if let Some(s) = &solid_model {
        initial.solid = Some(s.temperature.clone());
    }
    Ok(Prepared {
        design: DesignProblem {
            grid,
            spacing_m: spacing,
            step_s: step,
            periodic,
            map,
            drag_max_per_s: drag_max,
            drag_shape,
            origin_m: origin,
            registration,
            geometry_sampling,
        },
        units,
        model: Model { gamma, tau, transport, wall_heat, viscosity, solid: solid_model, steps },
        initial,
        problem: p.clone(),
        structure: porous,
    })
}

fn step_times(steps: usize, step_s: f64) -> Vec<f64> {
    (0..=steps).map(|n| n as f64 * step_s).collect()
}

#[derive(Clone, Debug)]
pub struct Solved {
    pub prepared: Prepared,
    pub raw: Vec<f64>,
    pub phi: Vec<f64>,
    pub exposure: Vec<f64>,
    pub history: History,
    pub structure: Option<StructureHistory>,
    pub diagnostics: Map<String, Value>,
}


pub fn structure_history(pr: &Prepared, history: &History) -> CaeResult<Option<StructureHistory>> {
    let Some(ps) = &pr.structure else { return Ok(None) };
    let impulses: Vec<Vec<[f64; 3]>> =
        history.diagnostics.iter().map(|d| d.cell_solid_drag_impulse.clone()).collect();
    let wall: Option<Vec<Vec<[f64; 3]>>> =
        history.diagnostics.iter().map(|d| d.wall_event_impulse.clone()).collect::<Option<Vec<_>>>();
    let temperatures =
        ps.expansion.as_ref().map(|_| history.solid_temperature_history(pr.units.temperature_unit_k));
    structure::history(ps, &impulses, wall.as_deref(), temperatures.as_deref()).map(Some)
}


pub fn solve(problem: &Value, design: &Design) -> CaeResult<Solved> {
    let raw = match design.get(COORDINATE) {
        Some(a) if design.len() == 1 => a,
        _ => return Err(err("Compressible LBM requires exactly model:control")),
    };
    let prepared = prepare(problem)?;
    let raw = prepared.design.checked_coordinate(raw)?;
    let (phi, exposure) = prepared.design.fields(&raw);
    let model = &prepared.model;
    if model.solid.is_none() {
        let t = &model.transport;
        if t.shape != prepared.design.grid.shape {
            return Err(err("Design and compressible transport grids differ"));
        }
        if t.periodic != prepared.design.periodic {
            return Err(err("Design filter periodicity must match transport"));
        }
    }
    let history = model.checked_history(&prepared.initial, &exposure, false)?;
    let p = &prepared.problem;
    let shape = prepared.design.grid.shape;
    let units = &prepared.units;
    let step = prepared.design.step_s;
    let force_scale = units.density_unit_kg_m3 * prepared.design.spacing_m.powi(4) / (step * step);
    let structure_result = structure_history(&prepared, &history)?;
    let mut structural = Map::new();
    if let (Some(ps), Some(h)) = (&prepared.structure, &structure_result) {
        structural = structure::admit(h, ps)?;
        if let Some(settings) = problem.get("fatigue").filter(|v| !v.is_null()) {
            let solved_solid =
                settings.get("temperature_source").and_then(Value::as_str) == Some("solved_solid");
            let temperature = if solved_solid { h.element_temperature_k.as_deref() } else { None };
            let mut fatigue =
                evaluate_rainflow(settings, &h.motion.stress_physical_pa, &h.time_s, temperature)?;
            if let Some(m) = fatigue.as_object_mut() {
                let source =
                    m.get("temperature_source").and_then(Value::as_str).unwrap_or_default().to_string();
                if let Some(lines) = m.get_mut("limitations").and_then(Value::as_array_mut) {
                    lines.retain(|l| !l.as_str().is_some_and(|s| s.starts_with("Solid temperature is")));
                    let wall_scope = if present(&prepared.problem, "slab_bounceback_axis") {
                        "selected slab normal/tangential wall impacts"
                    } else {
                        "selected specular wall-normal impacts"
                    };
                    lines.push(json!(format!(
                        "Solid temperature source: {source}. Solved temperatures require the explicit solid-to-element thermal-expansion map; gas temperatures are never substituted. Stress includes porous reaction and any {wall_scope}; continuum wall-stress and fatigue-life accuracy are not established by this transfer."
                    )));
                }
            }
            structural.insert("fatigue".into(), fatigue);
        }
    }
    let mut d = Map::new();
    d.insert("physical_qualification".into(), json!(false));
    if let Some(spec) = problem.get("environmental_ageing").filter(|v| !v.is_null()) {
        let temperatures = history.solid_temperature_history(units.temperature_unit_k);
        let rows: Vec<&[f64]> = temperatures.iter().map(Vec::as_slice).collect();
        let times = step_times(model.steps, step);
        d.insert("environmental_ageing".into(), ageing::observe(spec, shape, &times, None, &rows)?);
    }
    d.insert("geometry_sampling".into(), prepared.design.geometry_sampling.clone().unwrap_or(Value::Null));
    d.insert("porous_structure".into(), Value::Object(structural));
    d.insert(
        "wall_heat_energy_history_J".into(),
        json!(
            history
                .diagnostics
                .iter()
                .map(|s| s.wall_heat_energy.map(|v| v * units.energy_unit_j))
                .collect::<Vec<_>>()
        ),
    );
    d.insert("box_face_order".into(), json!(["x-", "x+", "y-", "y+", "z-", "z+"]));
    d.insert(
        "box_face_normal_force_history_N".into(),
        json!(
            history
                .diagnostics
                .iter()
                .map(|s| s.face_wall_normal_impulse.map(|v| v * force_scale))
                .collect::<Vec<_>>()
        ),
    );
    d.insert("accepted_intervals".into(), json!(model.steps));
    d.insert("history".into(), Value::Object(history.stacked(shape)));
    let _ = p;
    Ok(Solved { prepared, raw, phi, exposure, history, structure: structure_result, diagnostics: d })
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CompressibleLbmProvider;

impl CompressibleLbmProvider {
    pub const IMPLEMENTATION: &'static str =
        "implexity.lbm.compressible_provider.CompressibleLatticeBoltzmannProvider";


    pub fn declaration(problem: &Value) -> CaeResult<CouplingDeclaration> {
        prepare(problem)?;
        let p = problem.as_object().cloned().unwrap_or_default();
        let edge = |s: &str, t: &str, q: &str, reason: &str| {
            CouplingEdge::new(s, t, q, "one_way", true, reason).map_err(|e| err(e.0))
        };
        let solid = present(&p, "solid_thermal");
        let mut edges = Vec::new();
        if solid {
            edges.push(edge(
                "flow",
                "thermal",
                "gas_solid_heat_and_drag_dissipation",
                "Gas heat exchange and solid share of porous dissipation update stationary solid storage",
            )?);
            edges.push(edge(
                "thermal",
                "flow",
                "solid_to_gas_energy_exchange",
                "Carried solid temperature controls gas energy exchange and subsequent gas pressure",
            )?);
        }
        if present(&p, "viscosity") {
            edges.push(edge(
                "thermal",
                "flow",
                "gas_temperature_dependent_kinematic_viscosity",
                "Current gas temperature controls cellwise collision relaxation",
            )?);
        }
        let reservoir_patches = p
            .get("solid_thermal")
            .and_then(|s| s.get("reservoir_patches"))
            .is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty()));
        if reservoir_patches {
            edges.push(edge("environment", "thermal", "solid_surface_convection_radiation", "Prescribed thermal reservoir exchanges energy through half-cell conduction and an implicit solid-temperature update")?);
        }
        if p.get("wall_heat_flux_patches").is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty())) {
            edges.push(edge(
                "thermal",
                "flow",
                "prescribed_wall_heat",
                "Authored inward wall heat enters gas energy before optional gas-solid exchange",
            )?);
        }
        let mut physics: Vec<String> =
            if edges.is_empty() { vec!["flow".into()] } else { vec!["flow".into(), "thermal".into()] };
        if reservoir_patches {
            physics.push("environment".into());
        }
        if present(&p, "environmental_ageing") {
            physics.push("environmental_ageing".into());
            edges.push(edge(
                "thermal",
                "environmental_ageing",
                "solid_temperature_history",
                "Calibrated physical-time observer; chemical heat and material changes are not fed back",
            )?);
        }
        if present(&p, "porous_structure") {
            physics.push("mechanics".into());
            let ps = &p["porous_structure"];
            if ps.get("thermal_expansion").is_some_and(|v| !v.is_null()) {
                edges.push(edge(
                    "thermal",
                    "mechanics",
                    "solid_temperature_eigenstrain",
                    "Recorded solid temperatures drive linear expansion and elastic stress; no mechanical-work feedback",
                )?);
            }
            if ps.get("receive_porous_reaction").is_none_or(|v| v.as_bool() != Some(false)) {
                edges.push(edge(
                    "flow",
                    "mechanics",
                    "porous_reaction_impulse",
                    "Stationary porous drag reaction drives supported linear dynamics; no moving-medium or work feedback",
                )?);
            }
            if ps.get("wall_loads").is_some_and(|v| !v.is_null()) {
                let slab = present(&p, "slab_bounceback_axis");
                edges.push(edge(
                    "flow",
                    "mechanics",
                    if slab { "slab_wall_impulse" } else { "specular_wall_normal_impulse" },
                    "Selected localized wall impacts add conservative interval loads; slab impacts include tangential reaction, with resolved accuracy unqualified",
                )?);
            }
        }
        if present(&p, "fatigue") {
            physics.push("fatigue".into());
            edges.push(edge(
                "mechanics",
                "fatigue",
                "physical_stress_history",
                "Calibrated evaluation-only rainflow usage with prescribed or explicitly mapped solved-solid temperature admission",
            )?);
            if p["fatigue"].get("temperature_source").and_then(Value::as_str) == Some("solved_solid") {
                edges.push(edge(
                    "thermal",
                    "fatigue",
                    "solid_temperature_calibration_admission",
                    "Mapped solid-temperature history must remain within the supplied S-N calibration interval; no temperature-dependent life law",
                )?);
            }
        }
        Ok(CouplingDeclaration {
            provider: NAME.into(),
            active_physics: physics,
            edges,
            closed_loops: if solid { vec![vec!["flow".into(), "thermal".into()]] } else { Vec::new() },
            notes: notes(),
            ..CouplingDeclaration::default()
        })
    }
}

fn outputs(
    s: &Solved,
) -> CaeResult<(BTreeMap<String, f64>, BTreeMap<String, FieldValue>, Map<String, Value>)> {
    let pr = &s.prepared;
    let shape = pr.design.grid.shape.to_vec();
    let model = &pr.model;
    let units = &pr.units;
    let last = s.history.states.last().ok_or_else(|| err("empty compressible history"))?;
    let lattice = model.transport.lattice;
    let gas = gas_responses(&last.f, &last.g, &s.phi, model.gamma, lattice, units);
    let mut values: BTreeMap<String, f64> =
        GAS_RESPONSES.iter().zip(gas).map(|(k, v)| ((*k).to_string(), v)).collect();
    if present(&pr.problem, "reservoirs") {
        let ports = port_responses(&s.history, units, pr.design.step_s);
        values.extend(PORT_RESPONSES.iter().zip(ports).map(|(k, v)| ((*k).to_string(), v)));
    }
    let [rho, u, t, pressure, energy] = fields(&last.f, &last.g, model.gamma, lattice, units);
    let mut vshape = shape.clone();
    vshape.push(3);
    let mut out = BTreeMap::new();
    out.insert("density_kg_m3".to_string(), FieldValue::Array(array(rho, &shape)?));
    out.insert("velocity_m_s".to_string(), FieldValue::Array(array(u, &vshape)?));
    out.insert("temperature_K".to_string(), FieldValue::Array(array(t, &shape)?));
    out.insert("pressure_Pa".to_string(), FieldValue::Array(array(pressure, &shape)?));
    out.insert("total_energy_density_J_m3".to_string(), FieldValue::Array(array(energy, &shape)?));
    if let (Some(ps), Some(h)) = (&pr.structure, &s.structure) {
        values
            .insert(STRUCTURE_RESPONSES[0].into(), ps.mech.mean_squared_stress(&h.motion.stress_physical_pa));
    }
    let mut extra = Map::new();
    if let (Some(solid), Some(ts)) = (&model.solid, &last.solid) {
        let tu = units.temperature_unit_k;
        let eu = units.energy_unit_j;
        out.insert(
            "solid_temperature_K".into(),
            FieldValue::Array(array(ts.iter().map(|v| v * tu).collect(), &shape)?),
        );
        let n = ts.len() as f64;
        values.insert("mean_solid_temperature_K".into(), ts.iter().sum::<f64>() / n * tu);
        values.insert(
            "solid_stored_energy_change_J".into(),
            (0..ts.len()).map(|x| solid.capacity[x] * (ts[x] - solid.temperature[x])).sum::<f64>() * eu,
        );
        let conv: f64 = s
            .history
            .diagnostics
            .iter()
            .filter_map(|d| d.solid.as_ref())
            .map(|d| d.reservoir_convection_energy)
            .sum::<f64>()
            * eu;
        let rad: f64 = s
            .history
            .diagnostics
            .iter()
            .filter_map(|d| d.solid.as_ref())
            .map(|d| d.reservoir_radiation_energy)
            .sum::<f64>()
            * eu;
        values.insert("solid_convection_energy_J".into(), conv);
        values.insert("solid_radiation_energy_J".into(), rad);
        values.insert("solid_environment_energy_J".into(), conv + rad);
        let history: Vec<Value> =
            s.history.solid_temperature_history(tu).iter().map(|v| to_nested(v, &shape)).collect();
        extra.insert(
            "solid_thermal_history".into(),
            json!({
                "temperature_K": history,
                "time_s": (0..=model.steps).map(|i| i as f64 * pr.design.step_s).collect::<Vec<_>>(),
                "registration": pr.design.registration,
            }),
        );
        let diag: Vec<_> = s.history.diagnostics.iter().filter_map(|d| d.solid.as_ref()).collect();
        extra.insert(
            "solid_reservoir_exchange".into(),
            json!({
                "convection_energy_J": diag.iter().map(|d| d.reservoir_convection_energy * eu).collect::<Vec<_>>(),
                "radiation_energy_J": diag.iter().map(|d| d.reservoir_radiation_energy * eu).collect::<Vec<_>>(),
                "positive_direction": "into_solid",
                "maximum_residual_K": diag.iter().fold(f64::NEG_INFINITY, |m, d| m.max(d.reservoir_residual_k)),
            }),
        );
    }
    let units_of = |k: &str| match k {
        "fluid_fraction" => "1",
        "density_kg_m3" => "kg/m^3",
        "velocity_m_s" => "m/s",
        "temperature_K" | "solid_temperature_K" => "K",
        "pressure_Pa" => "Pa",
        _ => "J/m^3",
    };
    let mut metadata = Map::new();
    let order = [
        "density_kg_m3",
        "velocity_m_s",
        "temperature_K",
        "pressure_Pa",
        "total_energy_density_J_m3",
        "solid_temperature_K",
        "fluid_fraction",
    ];
    for key in order {
        if key != "fluid_fraction" && !out.contains_key(key) {
            continue;
        }
        let mut m = json!({"units": units_of(key), "rank": if key == "velocity_m_s" { "vector" } else { "scalar" }, "association": "cell", "registration": pr.design.registration});
        if key == "velocity_m_s"
            && let Value::Object(o) = &mut m
        {
            o.insert("components".into(), json!(["x", "y", "z"]));
        }
        metadata.insert(key.into(), m);
    }
    extra.insert("field_metadata".into(), Value::Object(metadata));
    out.insert("fluid_fraction".into(), FieldValue::Array(array(s.phi.clone(), &shape)?));
    Ok((values, out, extra))
}

impl LbmOperations for CompressibleLbmProvider {
    fn preflight_value(&self, problem: &Value, design: &Design) -> CaeResult<Map<String, Value>> {
        let s = solve(problem, design)?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("issues".into(), json!([]));
        out.extend(s.diagnostics);
        Ok(out)
    }

    fn evaluate_value(&self, problem: &Value, design: &Design) -> CaeResult<Evaluation> {
        let s = solve(problem, design)?;
        let (values, fields, extra) = outputs(&s)?;
        let mut diagnostics = s.diagnostics.clone();
        diagnostics.extend(extra);
        if let (Some(ps), Some(h), Some(Value::Object(m))) =
            (&s.prepared.structure, &s.structure, diagnostics.get_mut("porous_structure"))
        {
            m.insert("time_s".into(), json!(h.time_s));
            m.insert(
                "displacement_history_m".into(),
                json!(
                    h.motion
                        .full_displacement_m
                        .iter()
                        .map(|u| u.chunks(3).map(<[f64]>::to_vec).collect::<Vec<_>>())
                        .collect::<Vec<_>>()
                ),
            );
            m.insert("stress_history_Pa".into(), json!(h.motion.stress_physical_pa));
            m.insert("stress_components".into(), json!(["xx", "yy", "zz", "xy", "yz", "xz"]));
            m.insert("scope".into(), json!("One-way porous reaction plus selected wall impacts; slab includes tangential reaction but resolved wall-stress accuracy is unqualified; no mechanical-work feedback"));
            if let (Some(t), Some(e)) = (&h.element_temperature_k, &h.thermal_eigenstrain) {
                m.insert("element_temperature_history_K".into(), json!(t));
                m.insert("thermal_eigenstrain_history".into(), json!(e));
                m.insert("thermal_scope".into(), json!("Interval-start solid-temperature eigenstrain loads and endpoint elastic stress. The zero-eigenstrain mechanical quadratic energy treats these loads as external work, not a total coupled thermodynamic balance."));
            }
            if let Some(t) = &ps.wall_transfer {
                m.insert("selected_wall_event_indices".into(), json!(t.event_indices));
                let localization = s
                    .prepared
                    .model
                    .transport
                    .wall_events
                    .as_ref()
                    .map_or("legacy_single_image".to_string(), |w| w.localization.clone());
                m.insert("wall_event_localization".into(), json!(localization));
                m.insert("selected_wall_positions_m".into(), json!(t.map.cell_positions_m));
            }
        }
        Ok(Evaluation { provider: NAME.into(), responses: values, diagnostics, fields })
    }

    fn sensitivities_value(
        &self,
        problem: &Value,
        design: &Design,
        responses: &[String],
    ) -> CaeResult<DesignSensitivities> {
        let known = response_names();
        let unique: std::collections::BTreeSet<&String> = responses.iter().collect();
        if responses.is_empty()
            || unique.len() != responses.len()
            || responses.iter().any(|n| !known.contains(&n.as_str()))
        {
            return Err(err("Unique known compressible responses required"));
        }
        let p = problem.as_object().cloned().unwrap_or_default();
        let has = |list: &[&str]| responses.iter().any(|n| list.contains(&n.as_str()));
        if (has(&SOLID_RESPONSES) || has(&SOLID_RESERVOIR_RESPONSES)) && !present(&p, "solid_thermal") {
            return Err(err("Solid thermal responses require solid_thermal"));
        }
        if has(&STRUCTURE_RESPONSES) && !present(&p, "porous_structure") {
            return Err(err("Porous stress responses require porous_structure"));
        }
        if has(&PORT_RESPONSES) && !present(&p, "reservoirs") {
            return Err(err("Boundary flux responses require reservoirs"));
        }
        let s = solve(problem, design)?;
        if s.history
            .diagnostics
            .iter()
            .map(|d| d.minimum_sensor_switch_distance)
            .fold(f64::INFINITY, f64::min)
            <= 1e-8
        {
            return Err(err(
                "Kinetic sensor or positivity active-set switch prevents branch-local sensitivity admission",
            ));
        }
        let (values, _, _) = outputs(&s)?;
        let pr = &s.prepared;
        let model = &pr.model;
        let units = &pr.units;
        let last = s.history.states.last().ok_or_else(|| err("empty compressible history"))?;
        let lattice = model.transport.lattice;
        let n = s.phi.len();
        let mut out_values = BTreeMap::new();
        let mut gradients = BTreeMap::new();
        for name in responses {
            let mut terminal = State {
                f: vec![0.0; last.f.len()],
                g: vec![0.0; last.g.len()],
                solid: last.solid.as_ref().map(|t| vec![0.0; t.len()]),
            };
            let mut phi_bar = vec![0.0; n];
            let mut stages = vec![StageBar::default(); model.steps];
            let mut solid_temperature: Vec<Vec<f64>> = Vec::new();
            if let Some(i) = GAS_RESPONSES.iter().position(|r| r == name) {
                let mut w = [0.0; 9];
                w[i] = 1.0;
                let (fb, gb, pb) =
                    gas_responses_vjp(&last.f, &last.g, &s.phi, model.gamma, lattice, units, &w);
                terminal.f = fb;
                terminal.g = gb;
                phi_bar = pb;
            } else if let Some(i) = PORT_RESPONSES.iter().position(|r| r == name) {
                let mut w = [0.0; 10];
                w[i] = 1.0;
                let e = port_exchange_bar(&w, units, pr.design.step_s, model.steps);
                for st in &mut stages {
                    st.exchange = Some(e);
                }
            } else if name == "mean_solid_temperature_K" {
                terminal.solid = Some(vec![units.temperature_unit_k / n as f64; n]);
            } else if name == "solid_stored_energy_change_J" {
                let solid = model
                    .solid
                    .as_ref()
                    .ok_or_else(|| err("Solid thermal responses require solid_thermal"))?;
                terminal.solid = Some(solid.capacity.iter().map(|c| c * units.energy_unit_j).collect());
            } else if let Some(i) = SOLID_RESERVOIR_RESPONSES.iter().position(|r| r == name) {
                let e = units.energy_unit_j;
                let w = match i {
                    0 => [e, 0.0],
                    1 => [0.0, e],
                    _ => [e, e],
                };
                for st in &mut stages {
                    st.reservoir_energy = w;
                }
            } else {
                let (Some(ps), Some(h)) = (&pr.structure, &s.structure) else {
                    return Err(err("Porous stress responses require porous_structure"));
                };
                let (impulse, wall, temperature) = structure::stress_response_bar(ps, h, n)?;
                for (st, (ib, wb)) in
                    stages.iter_mut().zip(impulse.into_iter().zip(
                        wall.map_or_else(|| vec![None; model.steps], |w| w.into_iter().map(Some).collect()),
                    ))
                {
                    st.cell_impulse = Some(ib);
                    st.wall_event_impulse = wb;
                }
                if let Some(tb) = temperature {
                    let unit = units.temperature_unit_k;
                    solid_temperature =
                        tb.into_iter().map(|row| row.into_iter().map(|v| v * unit).collect()).collect();
                }
            }
            let cot = Cotangents { terminal, phi: phi_bar, stages, solid_temperature };
            let exposure_bar = model.adjoint(&s.history, &s.exposure, &cot)?;
            let d = &pr.design;
            let mut phi_total = cot.phi.clone();
            for x in 0..n {
                phi_total[x] += exposure_bar[x]
                    * resistance_derivative(d.step_s, d.drag_max_per_s, d.drag_shape, s.phi[x]);
            }
            let gradient = d.map.vjp(&s.raw, &phi_total);
            if !gradient.iter().all(|v| v.is_finite()) {
                return Err(err("Nonfinite compressible sensitivity"));
            }
            out_values.insert(name.clone(), values.get(name).copied().unwrap_or(f64::NAN));
            let mut g = Design::new();
            g.insert(COORDINATE, array(gradient, &d.grid.shape)?);
            gradients.insert(name.clone(), g);
        }
        Ok(DesignSensitivities { responses: out_values, gradients, diagnostics: s.diagnostics })
    }

    fn admission_value(&self, problem: &Value, candidate: &Design) -> CaeResult<Map<String, Value>> {
        Ok(solve(problem, candidate)?.diagnostics)
    }
}

crate::lbm_design_operations!(CompressibleLbmProvider, COORDINATE);

impl CaeProvider for CompressibleLbmProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn implementation(&self) -> &str {
        Self::IMPLEMENTATION
    }

    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        let names = response_names();
        let units: Vec<&str> = names.iter().map(|n| unit_of(n)).collect();
        let note_strings = notes();
        let note_refs: Vec<&str> = note_strings.iter().map(String::as_str).collect();
        let mut d = descriptor(
            NAME,
            &["compressible_kinetic_flow"],
            &names,
            &units,
            &[
                "fluid_fraction",
                "density_kg_m3",
                "velocity_m_s",
                "temperature_K",
                "pressure_Pa",
                "total_energy_density_J_m3",
                "solid_temperature_K",
            ],
            &note_refs,
        );
        for (name, meta) in &mut d.response_metadata {
            if let Value::Object(m) = meta {
                let n = name.as_str();
                if SOLID_RESPONSES.contains(&n) || SOLID_RESERVOIR_RESPONSES.contains(&n) {
                    m.insert("requires_problem_field".into(), json!("solid_thermal"));
                }
                if SOLID_RESERVOIR_RESPONSES.contains(&n) {
                    m.insert("description".into(), json!("Signed cumulative external solid heat: positive into solid, negative for cooling. Zero when no reservoir patches are configured."));
                }
                if PORT_RESPONSES.contains(&n) {
                    m.insert("requires_problem_field".into(), json!("reservoirs"));
                }
                if STRUCTURE_RESPONSES.contains(&n) {
                    m.insert("requires_problem_field".into(), json!("porous_structure"));
                }
            }
        }
        let data = presentation_data();
        let shape = [3usize, 3, 3];
        let editor = json!({
            "kind": "native_json",
            "title": data["title"],
            "problem_template": problem_template(),
            "schema": {"type": "object", "properties": data["properties"], "additionalProperties": false},
            "design_template": {COORDINATE: {"value": to_nested(&[1.0; 27], &shape), "lower": 0.0, "upper": 1.0, "designable": to_nested_bool(&[true; 27], &shape)}},
        });
        presentation(&d, editor, "array")
    }

    fn orchestration_contract(&self) -> Option<CaeResult<PublishedContract>> {
        let names = response_names();
        let units: Vec<&str> = names.iter().map(|n| unit_of(n)).collect();
        Some(
            contract(NAME, &names, &units, &["flow"], &notes())
                .map(|c| PublishedContract::Contract(Box::new(c))),
        )
    }

    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        prepare(problem)?;
        Ok(Arc::new(problem.clone()))
    }

    fn preflight(
        &self,
        problem: &ProviderProblem,
        _topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        prepare(problem_value(problem)?)?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("issues".into(), json!([]));
        out.insert("requires_complete_design".into(), json!(true));
        out.insert("physical_qualification".into(), json!(false));
        Ok(out)
    }

    fn evaluate(&self, _problem: &ProviderProblem, _topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        Err(missing_method("CompressibleLatticeBoltzmannProvider", "evaluate"))
    }

    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        _topology: &ArrayD<f64>,
        _response: &str,
    ) -> CaeResult<Sensitivity> {
        Err(missing_method("CompressibleLatticeBoltzmannProvider", "sensitivity"))
    }

    fn coupling_declaration(&self, problem: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        let problem = match problem.map(problem_value) {
            Some(Ok(v)) => v,
            Some(Err(e)) => return Some(Err(e)),
            None => return Some(Err(err("Compressible LBM coupling declaration requires a problem"))),
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
