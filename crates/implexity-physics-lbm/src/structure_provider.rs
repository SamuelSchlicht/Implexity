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
use implexity_linalg::dense::DenseMatrix;
use implexity_physics_solid::fatigue::{evaluate_rainflow, validate_rainflow_settings};
use implexity_physics_solid::structural_dynamics::{LinearAssembly, assemble_linear_tetrahedra};
use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use crate::d3q19::{C, Q};
use crate::design_ops::{
    Design, DesignSensitivities, LbmOperations, array, check_responses, presentation, problem_value,
};
use crate::geometry;
use crate::linear_structure::{Mechanics, Motion, dense_sub, well_conditioned};
use crate::nparray::{Kind, NdArray, asarray, py_int, py_real};
use crate::provider::{COORDINATE, contract, descriptor, missing_method};
use crate::solver::{LbmProblem, Tau};
use crate::thermal_provider::{
    self, HistoryCotangents, RESPONSES as THERMAL_RESPONSES, ThermalLbmProvider, ThermalProblem,
    ThermalSolved, UNITS as THERMAL_UNITS,
};
use crate::wall_exchange::{self, Layout};

pub const NAME: &str = "lattice_boltzmann_thermal_structure_d3q19";
pub const RESPONSES: [&str; 11] = [
    THERMAL_RESPONSES[0],
    THERMAL_RESPONSES[1],
    THERMAL_RESPONSES[2],
    THERMAL_RESPONSES[3],
    THERMAL_RESPONSES[4],
    THERMAL_RESPONSES[5],
    THERMAL_RESPONSES[6],
    THERMAL_RESPONSES[7],
    THERMAL_RESPONSES[8],
    "mean_squared_stress_Pa2",
    "mean_squared_displacement_m2",
];
pub const UNITS: [&str; 11] = [
    THERMAL_UNITS[0],
    THERMAL_UNITS[1],
    THERMAL_UNITS[2],
    THERMAL_UNITS[3],
    THERMAL_UNITS[4],
    THERMAL_UNITS[5],
    THERMAL_UNITS[6],
    THERMAL_UNITS[7],
    THERMAL_UNITS[8],
    "Pa^2",
    "m^2",
];

const PRESENTATION: &str = include_str!("structure_presentation.json");

const STRUCTURE_KEYS: [&str; 15] = [
    "nodes_m",
    "tetrahedra",
    "density_kg_m3",
    "young_Pa",
    "poisson",
    "fixed_dofs",
    "wall_link_indices",
    "trace_weights",
    "pressure_datum_Pa",
    "mass_damping_per_s",
    "stiffness_damping_s",
    "maximum_strain",
    "maximum_wall_motion_cells",
    "history_byte_budget",
    "provenance",
];

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
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

#[derive(Clone, Debug)]
pub struct Expansion {
    pub coefficient: Vec<f64>,
    pub reference: Vec<f64>,
    pub indices: Vec<[usize; 8]>,
    pub weights: Vec<[f64; 8]>,
    pub load_per_strain: Vec<Vec<f64>>,
    pub modulus: Vec<f64>,
    pub node_count: usize,
}

impl Expansion {
    #[must_use]
    pub fn strain(&self, temperature: &[Vec<f64>]) -> Vec<Vec<f64>> {
        temperature
            .iter()
            .map(|t| {
                (0..self.coefficient.len())
                    .map(|e| {
                        let sampled: f64 = (0..8).map(|k| t[self.indices[e][k]] * self.weights[e][k]).sum();
                        (sampled - self.reference[e]) * self.coefficient[e]
                    })
                    .collect()
            })
            .collect()
    }

    #[must_use]
    pub fn sampled(&self, temperature: &[Vec<f64>]) -> Vec<Vec<f64>> {
        temperature
            .iter()
            .map(|t| {
                (0..self.coefficient.len())
                    .map(|e| (0..8).map(|k| t[self.indices[e][k]] * self.weights[e][k]).sum())
                    .collect()
            })
            .collect()
    }
}


#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]
fn prepare_expansion(
    tp: &ThermalProblem,
    spec: &Value,
    nodes: &[[f64; 3]],
    tets: &[[usize; 4]],
    mech: &Mechanics,
    maximum_strain: f64,
) -> CaeResult<Expansion> {
    let (stress, volumes, young, poisson) = (&mech.stress, &mech.volumes, &mech.young, &mech.poisson);
    let keys_ok = spec.as_object().is_some_and(|m| {
        m.len() == 2 && m.contains_key("coefficient_per_K") && m.contains_key("stress_free_temperature_K")
    });
    if !keys_ok {
        return Err(err(
            "thermal_expansion requires per-element coefficient_per_K and stress_free_temperature_K",
        ));
    }
    let ne = tets.len();
    let mut values = BTreeMap::new();
    for name in ["coefficient_per_K", "stress_free_temperature_K"] {
        let v = asarray(&spec[name]);
        if v.shape != [ne] || !v.is_real() || !v.all_finite() {
            return Err(err("Thermal expansion parameters require finite per-element arrays"));
        }
        values.insert(name, v.data);
    }
    let reference = values.remove("stress_free_temperature_K").unwrap_or_default();
    let alpha = values.remove("coefficient_per_K").unwrap_or_default();
    let (t_min, t_max) = (tp.t.minimum_temperature_k, tp.t.maximum_temperature_k);
    if reference.iter().any(|r| *r < t_min || *r > t_max) {
        return Err(err("Stress-free temperature must lie inside thermal material validity"));
    }
    let limit = maximum_strain / 3f64.sqrt();
    if alpha.iter().zip(&reference).any(|(a, r)| a.abs() * (t_min - r).abs().max((t_max - r).abs()) > limit) {
        return Err(err("Thermal eigenstrain exceeds small-strain admission over the material interval"));
    }
    let p = &tp.p;
    let shape = p.shape();
    let mut indices = vec![[0usize; 8]; ne];
    let mut weights = vec![[0.0; 8]; ne];
    let mut lower = vec![[0i64; 3]; ne];
    let mut fraction = vec![[0.0; 3]; ne];
    for (e, t) in tets.iter().enumerate() {
        for a in 0..3 {
            let centre = (nodes[t[0]][a] + nodes[t[1]][a] + nodes[t[2]][a] + nodes[t[3]][a]) / 4.0;
            let q = (centre - p.origin_m[a]) / p.spacing_m - 0.5;
            let l = q.floor();
            lower[e][a] = l as i64;
            fraction[e][a] = q - l;
        }
    }
    for k in 0..8 {
        let corner = [(k >> 2) & 1, (k >> 1) & 1, k & 1];
        for e in 0..ne {
            let mut w = 1.0;
            let mut ix = [0i64; 3];
            for a in 0..3 {
                ix[a] = lower[e][a] + corner[a] as i64;
                w *= if corner[a] == 1 { fraction[e][a] } else { 1.0 - fraction[e][a] };
            }
            let active = w > 1e-12;
            if active && (0..3).any(|a| ix[a] < 0 || ix[a] >= shape[a] as i64) {
                return Err(err("Solid element centroid interpolation extends outside cell-centre grid"));
            }
            let c: [usize; 3] = std::array::from_fn(|a| ix[a].clamp(0, shape[a] as i64 - 1) as usize);
            let flat = (c[0] * shape[1] + c[1]) * shape[2] + c[2];
            if active && !p.solid_mask[flat] {
                return Err(err(
                    "Thermal expansion must sample fixed solid temperatures, never fluid or porous-design cells",
                ));
            }
            indices[e][k] = flat;
            weights[e][k] = w;
        }
    }
    let ndof = stress.ncols;
    let load_per_strain = (0..ne)
        .map(|e| {
            (0..ndof).map(|j| volumes[e] * (0..3).map(|i| stress.get(6 * e + i, j)).sum::<f64>()).collect()
        })
        .collect();
    let modulus = young.iter().zip(poisson).map(|(e, nu)| e / (1.0 - 2.0 * nu)).collect();
    Ok(Expansion {
        coefficient: alpha,
        reference,
        indices,
        weights,
        load_per_strain,
        modulus,
        node_count: nodes.len(),
    })
}

#[derive(Clone, Debug)]
pub struct Structure {
    pub mech: Mechanics,
    pub weights: Vec<f64>,
    pub indices: Vec<usize>,
    pub expansion: Option<Expansion>,
    pub pressure_datum_pa: f64,
    pub maximum_strain: f64,
    pub maximum_wall_motion_cells: f64,
    pub layout: Layout,
}

#[derive(Clone, Debug)]
pub struct Prepared {
    pub tp: ThermalProblem,
    pub a: Structure,
}


#[allow(clippy::too_many_lines, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn prepare(problem: &Value) -> CaeResult<Prepared> {
    let keys_ok = problem.as_object().is_some_and(|m| {
        let mut keys: Vec<&str> =
            m.keys().map(String::as_str).filter(|k| *k != "fatigue" && *k != "field_registration").collect();
        keys.sort_unstable();
        keys == ["fluid", "structure"]
    });
    let Some(root) = problem.as_object().filter(|_| keys_ok) else {
        return Err(err("LBM structure requires fluid and structure objects"));
    };
    if let Some(f) = root.get("fatigue") {
        validate_rainflow_settings(f)?;
    }
    let tp = ThermalProblem::normalize(&root["fluid"])?;
    let p = &tp.p;
    geometry::check_registration(root, p.shape(), p.spacing_m, p.origin_m)?;
    let s = &root["structure"];
    let keys_ok = s.as_object().is_some_and(|m| {
        let mut keys: Vec<&str> =
            m.keys().map(String::as_str).filter(|k| *k != "thermal_expansion").collect();
        keys.sort_unstable();
        let mut expected = STRUCTURE_KEYS.to_vec();
        expected.sort_unstable();
        keys == expected
    });
    if !keys_ok {
        return Err(err(
            "Structure must specify mesh, supports, transfer, material, damping and validity limits",
        ));
    }
    let nodes_a = asarray(&s["nodes_m"]);
    if nodes_a.shape.len() != 2 || nodes_a.shape[1] != 3 || !(4..=256).contains(&nodes_a.shape[0]) {
        return Err(err("Structural mesh requires 4–256 nodes; dense transient solve"));
    }
    if !s["provenance"].as_str().is_some_and(|v| !v.trim().is_empty()) {
        return Err(err("Structural material provenance required"));
    }
    let mut scalars = BTreeMap::new();
    for key in [
        "pressure_datum_Pa",
        "mass_damping_per_s",
        "stiffness_damping_s",
        "maximum_strain",
        "maximum_wall_motion_cells",
    ] {
        match py_real(&s[key]).filter(|v| v.is_finite()) {
            Some(v) => scalars.insert(key, v),
            None => return Err(err(format!("{key} must be a finite real scalar"))),
        };
    }
    let (mass_damping, stiffness_damping) = (scalars["mass_damping_per_s"], scalars["stiffness_damping_s"]);
    let (maximum_strain, maximum_motion) = (scalars["maximum_strain"], scalars["maximum_wall_motion_cells"]);
    if mass_damping.min(stiffness_damping) < 0.0
        || !(maximum_strain > 0.0 && maximum_strain <= 0.05)
        || !(maximum_motion > 0.0 && maximum_motion <= 0.01)
    {
        return Err(err(
            "Nonnegative damping, strain <=0.05 and fixed-wall displacement <=0.01 cells required",
        ));
    }
    let nn = nodes_a.shape[0];
    let fixed = asarray(&s["fixed_dofs"]);
    if fixed.kind != Kind::Bool || fixed.shape != [nn, 3] {
        return Err(err("fixed_dofs must be a boolean node-by-XYZ mask; supports have zero displacement"));
    }
    let free: Vec<usize> = fixed.bools().iter().enumerate().filter(|(_, f)| !**f).map(|(i, _)| i).collect();
    if free.is_empty() {
        return Err(err("At least one free structural DOF required"));
    }
    let (nodes, tets, assembly, material) = assemble(s, &nodes_a)?;
    let ne = tets.len();
    let ndof = 3 * nn;
    let mass = dense_sub(&assembly.mass.to_dense(), ndof, &free);
    let stiffness = dense_sub(&assembly.stiffness.to_dense(), ndof, &free);
    for (name, matrix) in [("mass", &mass), ("supported stiffness", &stiffness)] {
        if !well_conditioned(matrix)? {
            return Err(err(format!(
                "{name} must be positive definite and well conditioned; remove rigid body modes"
            )));
        }
    }
    let layout = wall_exchange::layout(p);
    let indices_a = asarray(&s["wall_link_indices"]);
    let nl = layout.positions.len();
    let unique: std::collections::BTreeSet<i64> = indices_a.data.iter().map(|v| *v as i64).collect();
    if indices_a.shape.len() != 1
        || indices_a.data.is_empty()
        || indices_a.kind != Kind::Int
        || indices_a.data.iter().any(|v| *v < 0.0 || *v >= nl as f64)
        || unique.len() != indices_a.data.len()
    {
        return Err(err("Unique valid fixed-wall link indices required"));
    }
    let indices: Vec<usize> = indices_a.data.iter().map(|v| *v as usize).collect();
    let selected: Vec<f64> = indices.iter().flat_map(|&i| layout.positions[i]).collect();
    let positions = NdArray { shape: vec![indices.len(), 3], kind: Kind::Float, data: selected };
    let weights = wall_exchange::validate_projection(&positions, &nodes_a, &asarray(&s["trace_weights"]))?;
    let required =
        8 * ((p.step_count + 1) * (nn * 6 + ne * 6) + p.step_count * nl * 3 + free.len() * free.len() * 3);
    let budget = py_int(&s["history_byte_budget"]);
    if budget.is_none_or(|b| b < i64::try_from(required).unwrap_or(i64::MAX)) {
        return Err(err(format!(
            "Structural history/matrices need at least {required} bytes, excluding AD workspace"
        )));
    }
    let stress = DenseMatrix { nrows: 6 * ne, ncols: ndof, data: assembly.stress.to_dense() };
    let damping = DenseMatrix {
        nrows: free.len(),
        ncols: free.len(),
        data: mass
            .data
            .iter()
            .zip(&stiffness.data)
            .map(|(m, k)| mass_damping * m + stiffness_damping * k)
            .collect(),
    };
    let [_, young, poisson] = material;
    let mech = Mechanics {
        free,
        mass,
        stiffness,
        damping,
        stress,
        volumes: assembly.volumes,
        nodes: nn,
        young,
        poisson,
    };
    let expansion = match s.get("thermal_expansion") {
        Some(spec) => Some(prepare_expansion(&tp, spec, &nodes, &tets, &mech, maximum_strain)?),
        None => None,
    };
    let solved_solid = root
        .get("fatigue")
        .and_then(|f| f.get("temperature_source"))
        .is_some_and(|v| v.as_str() == Some("solved_solid"));
    if solved_solid && expansion.is_none() {
        return Err(err("Solved-solid fatigue requires the explicit thermal-expansion temperature map"));
    }
    Ok(Prepared {
        a: Structure {
            mech,
            weights,
            indices,
            expansion,
            pressure_datum_pa: scalars["pressure_datum_Pa"],
            maximum_strain,
            maximum_wall_motion_cells: maximum_motion,
            layout,
        },
        tp,
    })
}


#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn assemble(
    s: &Value,
    nodes_a: &NdArray,
) -> CaeResult<(Vec<[f64; 3]>, Vec<[usize; 4]>, LinearAssembly, [Vec<f64>; 3])> {
    let nn = nodes_a.shape.first().copied().unwrap_or(0);
    if nodes_a.shape.len() != 2
        || nodes_a.shape[1] != 3
        || !matches!(nodes_a.kind, Kind::Float | Kind::Int)
        || !nodes_a.all_finite()
    {
        return Err(err("points require finite numerical [node,3] metre coordinates"));
    }
    let nodes: Vec<[f64; 3]> = nodes_a.data.chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
    let tets_a = asarray(&s["tetrahedra"]);
    if tets_a.shape.len() != 2 || tets_a.shape[1] != 4 || tets_a.shape[0] == 0 || tets_a.kind != Kind::Int {
        return Err(err("elements require integer [element,4] node indices"));
    }
    let mut used = vec![false; nn];
    for v in &tets_a.data {
        if *v < 0.0 || *v >= nn as f64 {
            return Err(err("invalid connectivity or unused mesh nodes"));
        }
        used[*v as usize] = true;
    }
    if used.iter().any(|u| !u) {
        return Err(err("invalid connectivity or unused mesh nodes"));
    }
    let tets: Vec<[usize; 4]> =
        tets_a.data.chunks(4).map(|c| [c[0] as usize, c[1] as usize, c[2] as usize, c[3] as usize]).collect();
    let ne = tets.len();
    let mut material = Vec::new();
    for (name, key) in
        [("density", "density_kg_m3"), ("Young modulus", "young_Pa"), ("Poisson ratio", "poisson")]
    {
        let a = asarray(&s[key]);
        if a.shape != [ne] || !matches!(a.kind, Kind::Float | Kind::Int) || !a.all_finite() {
            return Err(err(format!("{name} requires one finite real value per tetrahedron")));
        }
        material.push(a.data);
    }
    let assembly = assemble_linear_tetrahedra(&nodes, &tets, &material[0], &material[1], &material[2])?;
    let [rho, young, poisson]: [Vec<f64>; 3] = material.try_into().unwrap_or_default();
    Ok((nodes, tets, assembly, [rho, young, poisson]))
}

#[derive(Clone, Debug)]
pub struct Response {
    pub positions_m: Vec<[f64; 3]>,
    pub interval_force_n: Vec<Vec<[f64; 3]>>,
    pub time_s: Vec<f64>,
    pub nodal_force_n: Vec<Vec<f64>>,
    pub motion: Motion,
    pub thermal_strain: Option<Vec<Vec<f64>>>,
}


pub fn coupled_history(pr: &Prepared, solved: &ThermalSolved) -> CaeResult<Response> {
    let tp = &pr.tp;
    let a = &pr.a;
    let p = &tp.p;
    let steps = p.step_count;
    let alpha = &solved.model.alpha;
    let traj = &solved.trajectory;
    let mut interval_force = Vec::with_capacity(steps);
    for n in 0..steps {
        let tau = tp.relaxation(&traj.temperature[n]);
        let loads = wall_exchange::loads(p, alpha, &traj.f[n], a.pressure_datum_pa, Tau::PerCell(&tau));
        interval_force.push(a.indices.iter().map(|&i| loads.force_n[i]).collect::<Vec<_>>());
    }
    let time_s: Vec<f64> = (0..=steps).map(|n| n as f64 * p.step_s).collect();
    let strain = a.expansion.as_ref().map(|x| x.strain(&traj.temperature));
    let nn = a.mech.nodes;
    let nodal: Vec<Vec<f64>> = interval_force
        .iter()
        .enumerate()
        .map(|(n, forces)| {
            let mut row = link_to_nodes(&a.weights, forces, nn);
            if let (Some(x), Some(strain)) = (&a.expansion, &strain) {
                for (j, value) in row.iter_mut().enumerate() {
                    *value +=
                        (0..x.coefficient.len()).map(|e| strain[n][e] * x.load_per_strain[e][j]).sum::<f64>();
                }
            }
            row
        })
        .collect();
    let thermal_stress = match (&a.expansion, &strain) {
        (Some(x), Some(strain)) => Some(
            strain
                .iter()
                .map(|row| row.iter().zip(&x.modulus).map(|(s, m)| s * m).collect())
                .collect::<Vec<_>>(),
        ),
        _ => None,
    };
    let motion = a.mech.respond(&nodal, &time_s, thermal_stress.as_deref())?;
    Ok(Response {
        positions_m: a.indices.iter().map(|&i| a.layout.positions[i]).collect(),
        interval_force_n: interval_force,
        time_s,
        nodal_force_n: nodal,
        motion,
        thermal_strain: strain,
    })
}

#[must_use]
pub fn link_to_nodes(weights: &[f64], forces: &[[f64; 3]], nodes: usize) -> Vec<f64> {
    let mut row = vec![0.0; 3 * nodes];
    for (l, f) in forces.iter().enumerate() {
        for node in 0..nodes {
            let w = weights[l * nodes + node];
            for d in 0..3 {
                row[3 * node + d] += w * f[d];
            }
        }
    }
    row
}

#[must_use]
pub fn nodes_to_links(weights: &[f64], nodal_bar: &[f64], links: usize, nodes: usize) -> Vec<[f64; 3]> {
    (0..links)
        .map(|l| {
            std::array::from_fn(|d| {
                (0..nodes).map(|node| weights[l * nodes + node] * nodal_bar[3 * node + d]).sum()
            })
        })
        .collect()
}

#[must_use]
pub fn structure_responses(a: &Structure, r: &Response) -> [f64; 2] {
    let full = &r.motion.full_displacement_m;
    let count = full.len() * 3 * a.mech.nodes;
    let displacement = full.iter().flatten().map(|u| u * u).sum::<f64>() / count as f64;
    [a.mech.mean_squared_stress(&r.motion.stress_physical_pa), displacement]
}


pub fn structure_cotangents(pr: &Prepared, r: &Response, which: usize) -> CaeResult<HistoryCotangents> {
    let a = &pr.a;
    let p = &pr.tp.p;
    let nt = r.time_s.len();
    let steps = nt - 1;
    let ne = a.mech.elements();
    let nn = a.mech.nodes;
    let ndof = 3 * nn;
    let (full_bar, stress_bar) = if which == 0 {
        (vec![vec![0.0; ndof]; nt], Some(a.mech.mean_squared_stress_bar(&r.motion.stress_physical_pa)))
    } else {
        let count = (nt * ndof) as f64;
        (
            r.motion
                .full_displacement_m
                .iter()
                .map(|u| u.iter().map(|v| 2.0 * v / count).collect())
                .collect(),
            None,
        )
    };
    let (nodal_bar, thermal_bar) = a.mech.reverse(&r.time_s, full_bar, stress_bar.as_deref())?;
    let mut post = Vec::with_capacity(steps);
    let mut strain_bar = vec![vec![0.0; ne]; nt];
    if let Some(x) = &a.expansion {
        for t in 0..nt {
            for e in 0..ne {
                strain_bar[t][e] = thermal_bar[t][e] * x.modulus[e];
            }
        }
    }
    for n in 0..steps {
        let selected = nodes_to_links(&a.weights, &nodal_bar[n], a.indices.len(), nn);
        let mut link_bar = vec![[0.0; 3]; a.layout.positions.len()];
        for (l, &link) in a.indices.iter().enumerate() {
            link_bar[link] = selected[l];
        }
        post.push(wall_exchange::loads_from_post_vjp(p, &a.layout, &link_bar));
        if let Some(x) = &a.expansion {
            for e in 0..ne {
                strain_bar[n][e] +=
                    x.load_per_strain[e].iter().zip(&nodal_bar[n]).map(|(l, b)| l * b).sum::<f64>();
            }
        }
    }
    let mut temperature = vec![Vec::new(); nt];
    if let Some(x) = &a.expansion {
        let cells = p.cells();
        for t in 0..nt {
            let mut tb = vec![0.0; cells];
            for e in 0..ne {
                let g = strain_bar[t][e] * x.coefficient[e];
                for k in 0..8 {
                    tb[x.indices[e][k]] += g * x.weights[e][k];
                }
            }
            temperature[t] = tb;
        }
    }
    Ok(HistoryCotangents { post, temperature })
}

#[derive(Clone, Debug)]
pub struct Solved {
    pub prepared: Prepared,
    pub thermal: ThermalSolved,
    pub response: Response,
    pub diagnostics: Map<String, Value>,
}


pub fn solve(problem: &Value, design: &Design) -> CaeResult<Solved> {
    let prepared = prepare(problem)?;
    let thermal = ThermalProblem::solve(&problem["fluid"], design)?;
    let response = coupled_history(&prepared, &thermal)?;
    if !response.motion.is_finite() || !response.nodal_force_n.iter().flatten().all(|v| v.is_finite()) {
        return Err(err("Nonfinite LBM structural response"));
    }
    let a = &prepared.a;
    let p = &prepared.tp.p;
    let (elastic, total) =
        a.mech.strain_measures(&response.motion.stress_physical_pa, response.thermal_strain.as_deref());
    let strain = elastic.max(total);
    let motion = a.mech.motion(&response.motion) / p.spacing_m;
    if strain > a.maximum_strain || motion > a.maximum_wall_motion_cells {
        return Err(err("Linear-strain or fixed-wall displacement limit exceeded"));
    }
    let error = response.motion.energy_error();
    if error > 1e-8 {
        return Err(err("Mechanical energy balance failed"));
    }
    let mut diagnostics = thermal.diagnostics.clone();
    diagnostics.insert("maximum_strain".into(), json!(strain));
    diagnostics.insert("maximum_wall_motion_cells".into(), json!(motion));
    diagnostics.insert("mechanical_energy_relative_error".into(), json!(error));
    diagnostics.insert(
        "structural_coupling".into(),
        json!("one-way interval loads; fixed wall; zero initial motion"),
    );
    diagnostics.insert("thermal_expansion_enabled".into(), json!(a.expansion.is_some()));
    diagnostics.insert(
        "mechanical_energy_scope".into(),
        json!(
            "Kinetic plus zero-eigenstrain quadratic elastic energy; thermal equivalent loads counted as external work, not a coupled total-energy certification"
        ),
    );
    if let Some(settings) = problem.get("fatigue") {
        let solved_temperature =
            if settings.get("temperature_source").and_then(Value::as_str) == Some("solved_solid") {
                a.expansion.as_ref().map(|x| x.sampled(&thermal.trajectory.temperature))
            } else {
                None
            };
        let mut fatigue = evaluate_rainflow(
            settings,
            &response.motion.stress_physical_pa,
            &response.time_s,
            solved_temperature.as_deref(),
        )?;
        if let Some(lines) = fatigue.get_mut("limitations").and_then(Value::as_array_mut) {
            lines.retain(|l| !l.as_str().is_some_and(|s| s.starts_with("Solid temperature is")));
            lines.push(json!(if solved_temperature.is_some() {
                "Solid temperatures are mapped from solved fixed-solid cells, never from fluid cells."
            } else {
                "Solid temperature is prescribed independently; it is not the LBM fluid temperature or a solved solid-temperature history."
            }));
        }
        diagnostics.insert("fatigue".into(), fatigue);
    }
    Ok(Solved { prepared, thermal, response, diagnostics })
}

#[must_use]
pub fn structure_template(tp: &ThermalProblem) -> Value {
    let p = &tp.p;
    let shape = p.shape();
    let (lx, lz, h) = (shape[0] as f64 * p.spacing_m, shape[2] as f64 * p.spacing_m, p.spacing_m);
    let mut nodes = Vec::new();
    for x in [0.0, lx] {
        for y in [-h, 0.0] {
            for z in [0.0, lz] {
                nodes.push([x, y, z]);
            }
        }
    }
    let mut tets = [[0usize, 4, 6, 7], [0, 6, 2, 7], [0, 2, 3, 7], [0, 3, 1, 7], [0, 1, 5, 7], [0, 5, 4, 7]];
    for t in &mut tets {
        let e: [[f64; 3]; 3] =
            std::array::from_fn(|r| std::array::from_fn(|c| nodes[t[r + 1]][c] - nodes[t[0]][c]));
        let det = e[0][0] * (e[1][1] * e[2][2] - e[1][2] * e[2][1])
            - e[0][1] * (e[1][0] * e[2][2] - e[1][2] * e[2][0])
            + e[0][2] * (e[1][0] * e[2][1] - e[1][1] * e[2][0]);
        if det < 0.0 {
            t.swap(1, 2);
        }
    }
    let layout = wall_exchange::layout(p);
    let ids: Vec<usize> = (0..layout.cells.len())
        .filter(|&i| p.grid.coords(layout.cells[i])[1] == 0 && C[layout.links[i]][1] == 1)
        .collect();
    let weights: Vec<Vec<f64>> = ids
        .iter()
        .map(|&i| {
            let x = layout.positions[i][0] / lx;
            let z = layout.positions[i][2] / lz;
            let mut w = vec![0.0; 8];
            w[2] = (1.0 - x) * (1.0 - z);
            w[3] = (1.0 - x) * z;
            w[6] = x * (1.0 - z);
            w[7] = x * z;
            w
        })
        .collect();
    let fixed: Vec<[bool; 3]> = nodes.iter().map(|n| [n[1] < 0.0; 3]).collect();
    json!({
        "nodes_m": nodes, "tetrahedra": tets, "density_kg_m3": vec![7800.0; 6], "young_Pa": vec![1e6; 6],
        "poisson": vec![0.3; 6], "fixed_dofs": fixed, "wall_link_indices": ids, "trace_weights": weights,
        "pressure_datum_Pa": 0.0, "mass_damping_per_s": 0.1, "stiffness_damping_s": 0.0, "maximum_strain": 0.01,
        "maximum_wall_motion_cells": 0.001, "history_byte_budget": 32_000_000,
        "provenance": "Synthetic elastic slab; replace with calibrated material and resolved mesh",
    })
}


pub fn editor() -> CaeResult<Value> {
    let base = thermal_provider::editor();
    let fluid = base["problem_template"].clone();
    let tp = ThermalProblem::normalize(&fluid)?;
    let data = presentation_data();
    Ok(json!({
        "kind": "native_json",
        "title": data["title"],
        "problem_template": {"fluid": fluid, "structure": structure_template(&tp)},
        "design_template": base["design_template"],
        "schema": data["schema"],
    }))
}

fn fields(s: &Solved) -> CaeResult<BTreeMap<String, FieldValue>> {
    let p = &s.prepared.tp.p;
    let steps = p.step_count;
    let traj = &s.thermal.trajectory;
    let shape = p.shape().to_vec();
    let (rho, u) = p.fields(&s.thermal.model.alpha, &traj.f[steps]);
    let mut vshape = shape.clone();
    vshape.push(3);
    let mut hshape = vec![steps + 1];
    hshape.extend_from_slice(&shape);
    let r = &s.response;
    let nl = r.positions_m.len();
    let nn = s.prepared.a.mech.nodes;
    let ne = s.prepared.a.mech.elements();
    let flat3 = |v: &[[f64; 3]]| v.iter().flatten().copied().collect::<Vec<f64>>();
    let mut out = BTreeMap::new();
    out.insert("fluid_fraction".into(), FieldValue::Array(array(s.thermal.model.phi.clone(), &shape)?));
    out.insert("density_kg_m3".into(), FieldValue::Array(array(rho, &shape)?));
    out.insert("velocity_m_s".into(), FieldValue::Array(array(u, &vshape)?));
    out.insert("temperature_K".into(), FieldValue::Array(array(traj.temperature[steps].clone(), &shape)?));
    out.insert(
        "temperature_history_K".into(),
        FieldValue::Array(array(traj.temperature.iter().flatten().copied().collect(), &hshape)?),
    );
    out.insert("time_s".into(), FieldValue::Array(array(r.time_s.clone(), &[steps + 1])?));
    out.insert("wall_link_positions_m".into(), FieldValue::Array(array(flat3(&r.positions_m), &[nl, 3])?));
    out.insert(
        "wall_link_force_N".into(),
        FieldValue::Array(array(flat3(&r.interval_force_n[steps - 1]), &[nl, 3])?),
    );
    out.insert(
        "solid_displacement_history_m".into(),
        FieldValue::Array(array(
            r.motion.full_displacement_m.iter().flatten().copied().collect(),
            &[steps + 1, nn, 3],
        )?),
    );
    out.insert(
        "solid_stress_history_Pa".into(),
        FieldValue::Array(array(
            r.motion.stress_physical_pa.iter().flatten().flatten().copied().collect(),
            &[steps + 1, ne, 6],
        )?),
    );
    out.insert(
        "selected_wall_force_history_N".into(),
        FieldValue::Array(array(
            r.interval_force_n.iter().flatten().flatten().copied().collect(),
            &[steps, nl, 3],
        )?),
    );
    if let Some(usage) = s.diagnostics.get("fatigue").and_then(|f| f.get("usage_per_element")) {
        out.insert("fatigue_usage_per_element".into(), FieldValue::Json(usage.clone()));
    }
    Ok(out)
}

fn response_vector(s: &Solved) -> Vec<f64> {
    let mut v = s.thermal.tp.vector(&s.thermal.model, &s.thermal.trajectory).to_vec();
    v.extend(structure_responses(&s.prepared.a, &s.response));
    v
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ThermalStructureLbmProvider;

impl ThermalStructureLbmProvider {
    pub const IMPLEMENTATION: &'static str = "implexity.lbm.structure_provider.ThermalStructureLBMProvider";


    pub fn declaration(problem: &Value) -> CaeResult<CouplingDeclaration> {
        let mut d = ThermalLbmProvider::declaration(&problem["fluid"])?;
        d.provider = NAME.into();
        d.notes = notes();
        let edge = |s: &str, t: &str, q: &str, reason: &str| {
            CouplingEdge::new(s, t, q, "one_way", true, reason).map_err(|e| err(e.0))
        };
        d.active_physics.push("mechanics".into());
        d.edges.push(edge(
            "flow",
            "mechanics",
            "fixed_wall_momentum_exchange_history",
            "Conservative interval loads drive linear structural dynamics",
        )?);
        if problem["structure"].get("thermal_expansion").is_some() {
            d.edges.push(edge(
                "thermal",
                "mechanics",
                "solid_temperature_eigenstrain",
                "Solid-cell temperature drives isotropic thermal strain and consistent nodal loads",
            )?);
        }
        if let Some(f) = problem.get("fatigue") {
            validate_rainflow_settings(f)?;
            d.active_physics.push("fatigue".into());
            d.edges.push(edge(
                "mechanics",
                "fatigue",
                "physical_stress_history",
                "Calibrated nondifferentiable rainflow usage observer",
            )?);
            if f.get("temperature_source").and_then(Value::as_str) == Some("solved_solid") {
                d.edges.push(edge(
                    "thermal",
                    "fatigue",
                    "solid_temperature_calibration_admission",
                    "Mapped fixed-solid temperature history must remain within the supplied S-N calibration interval; no temperature-dependent life law",
                )?);
            }
        }
        Ok(d)
    }
}

impl LbmOperations for ThermalStructureLbmProvider {
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
        let values = response_vector(&s);
        let fields = fields(&s)?;
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
        check_responses(responses, &RESPONSES, "Unknown or duplicate structural LBM responses")?;
        let s = solve(problem, design)?;
        let values = response_vector(&s);
        let th = &s.thermal;
        let n = th.tp.p.cells();
        let mut out_values = BTreeMap::new();
        let mut gradients = BTreeMap::new();
        for name in responses {
            let index = RESPONSES.iter().position(|r| r == name).unwrap_or(0);
            let gradient = if index < THERMAL_RESPONSES.len() {
                let (f, t, c, a, phi) = thermal_provider::response_cotangents(th, index);
                th.tp.adjoint(&th.raw, &th.model, &th.trajectory, f, t, c, a, phi)?
            } else {
                let extra = structure_cotangents(&s.prepared, &s.response, index - THERMAL_RESPONSES.len())?;
                th.tp.adjoint_history(
                    &th.raw,
                    &th.model,
                    &th.trajectory,
                    vec![0.0; n * Q],
                    vec![0.0; n],
                    vec![0.0; n],
                    vec![0.0; n],
                    vec![0.0; n],
                    &extra,
                )?
            };
            if !gradient.iter().all(|v| v.is_finite()) {
                return Err(err("Nonfinite coupled structural derivative"));
            }
            out_values.insert(name.clone(), values[index]);
            let mut g = Design::new();
            g.insert(COORDINATE, array(gradient, &th.tp.p.shape())?);
            gradients.insert(name.clone(), g);
        }
        Ok(DesignSensitivities { responses: out_values, gradients, diagnostics: s.diagnostics })
    }

    fn admission_value(&self, problem: &Value, candidate: &Design) -> CaeResult<Map<String, Value>> {
        Ok(solve(problem, candidate)?.diagnostics)
    }
}

crate::lbm_design_operations!(ThermalStructureLbmProvider, COORDINATE);

impl CaeProvider for ThermalStructureLbmProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn implementation(&self) -> &str {
        Self::IMPLEMENTATION
    }

    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        let data = presentation_data();
        let strings = |key: &str| -> Vec<String> {
            data[key]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };
        let analyses = strings("analyses");
        let fields = strings("fields");
        let notes = strings("notes");
        let analyses: Vec<&str> = analyses.iter().map(String::as_str).collect();
        let fields: Vec<&str> = fields.iter().map(String::as_str).collect();
        let notes: Vec<&str> = notes.iter().map(String::as_str).collect();
        let d = descriptor(NAME, &analyses, &RESPONSES, &UNITS, &fields, &notes);
        presentation(&d, editor()?, "array")
    }

    fn orchestration_contract(&self) -> Option<CaeResult<PublishedContract>> {
        Some(
            contract(NAME, &RESPONSES, &UNITS, &["flow", "thermal", "mechanics"], &notes())
                .map(|c| PublishedContract::Contract(Box::new(c))),
        )
    }

    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        let pr = prepare(problem)?;
        let map = problem.as_object().cloned().unwrap_or_default();
        let p: &LbmProblem = &pr.tp.p;
        Ok(Arc::new(geometry::registered_problem(&map, p.shape(), p.spacing_m, p.origin_m)?))
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
        Err(missing_method("ThermalStructureLBMProvider", "evaluate"))
    }

    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        _topology: &ArrayD<f64>,
        _response: &str,
    ) -> CaeResult<Sensitivity> {
        Err(missing_method("ThermalStructureLBMProvider", "sensitivity"))
    }

    fn coupling_declaration(&self, problem: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        let problem = match problem.map(problem_value) {
            Some(Ok(v)) => v,
            Some(Err(e)) => return Some(Err(e)),
            None => return Some(Err(err("Structural LBM coupling declaration requires a problem"))),
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
