// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value, json};

use implexity_core::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;

use crate::linear_structure::{Mechanics, Motion, dense_sub, well_conditioned};
use crate::nparray::{Kind, NdArray, asarray, py_int};
use crate::structure_provider::{assemble, link_to_nodes, nodes_to_links};
use crate::wall_exchange::validate_projection;

use super::transport::{Transport, TransportKind, WallEvents, box_wall_events, slab_wall_events};

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Debug)]
pub struct TransferMap {
    pub weights: Vec<f64>,
    pub cell_positions_m: Vec<[f64; 3]>,
    pub nodes: usize,
    pub dt_s: f64,
    pub impulse_unit_n_s: f64,
    pub energy_unit_j: f64,
}

impl TransferMap {
    #[must_use]
    pub fn rows(&self) -> usize {
        self.cell_positions_m.len()
    }
}


pub fn porous_transfer_map(
    positions: &NdArray,
    nodes: &NdArray,
    weights: &NdArray,
    dx: f64,
    dt: f64,
    density_unit: &Value,
) -> CaeResult<TransferMap> {
    let rho = asarray(density_unit).real_scalar();
    if !(dx.is_finite() && dx > 0.0 && dt.is_finite() && dt > 0.0)
        || !rho.is_some_and(|r| r.is_finite() && r > 0.0)
    {
        return Err(err("Positive finite lattice length, time and density scales required"));
    }
    let rho = rho.unwrap_or(1.0);
    let projection = validate_projection(positions, nodes, weights)?;
    Ok(TransferMap {
        weights: projection,
        cell_positions_m: positions.data.chunks(3).map(|c| [c[0], c[1], c[2]]).collect(),
        nodes: nodes.shape[0],
        dt_s: dt,
        impulse_unit_n_s: rho * dx.powi(4) / dt,
        energy_unit_j: rho * dx.powi(5) / (dt * dt),
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct NodalTransfer {
    pub nodal_impulse_n_s: Vec<[f64; 3]>,
    pub nodal_heat_j: Vec<f64>,
    pub nodal_force_n: Vec<[f64; 3]>,
    pub nodal_heat_power_w: Vec<f64>,
}


pub fn porous_nodal_transfer(
    impulse: &[[f64; 3]],
    heat: &[f64],
    m: &TransferMap,
) -> CaeResult<NodalTransfer> {
    if impulse.len() != heat.len() || heat.len() != m.rows() {
        return Err(err("Cell transfers must match the conservative projection"));
    }
    let nn = m.nodes;
    let nodal = link_to_nodes(&m.weights, impulse, nn);
    let nodal_impulse: Vec<[f64; 3]> =
        (0..nn).map(|k| std::array::from_fn(|d| nodal[3 * k + d] * m.impulse_unit_n_s)).collect();
    let nodal_heat: Vec<f64> = (0..nn)
        .map(|k| (0..heat.len()).map(|r| m.weights[r * nn + k] * heat[r]).sum::<f64>() * m.energy_unit_j)
        .collect();
    Ok(NodalTransfer {
        nodal_force_n: nodal_impulse.iter().map(|v| v.map(|x| x / m.dt_s)).collect(),
        nodal_heat_power_w: nodal_heat.iter().map(|v| v / m.dt_s).collect(),
        nodal_impulse_n_s: nodal_impulse,
        nodal_heat_j: nodal_heat,
    })
}

#[derive(Clone, Debug)]
pub struct WallTransfer {
    pub map: TransferMap,
    pub event_indices: Vec<usize>,
    pub event_count: usize,
}


#[allow(clippy::too_many_arguments, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn wall_transfer_map(
    events: &WallEvents,
    indices: &NdArray,
    nodes: &NdArray,
    weights: &NdArray,
    origin: [f64; 3],
    dx: f64,
    dt: f64,
    density_unit: &Value,
) -> CaeResult<WallTransfer> {
    if !origin.iter().all(|v| v.is_finite()) {
        return Err(err("Finite XYZ wall-map origin required"));
    }
    let count = events.positions_lattice.len();
    let unique: std::collections::BTreeSet<i64> = indices.data.iter().map(|v| *v as i64).collect();
    if indices.shape.len() != 1
        || indices.data.is_empty()
        || indices.kind != Kind::Int
        || indices.data.iter().any(|v| *v < 0.0 || *v >= count as f64)
        || unique.len() != indices.data.len()
    {
        return Err(err("Unique valid wall-event indices required"));
    }
    let selected: Vec<usize> = indices.data.iter().map(|v| *v as usize).collect();
    let positions: Vec<f64> = selected
        .iter()
        .flat_map(|&i| (0..3).map(move |a| (i, a)))
        .map(|(i, a)| origin[a] + (events.positions_lattice[i][a] + 0.5) * dx)
        .collect();
    let positions = NdArray { shape: vec![selected.len(), 3], kind: Kind::Float, data: positions };
    let map = porous_transfer_map(&positions, nodes, weights, dx, dt, density_unit)?;
    Ok(WallTransfer { map, event_indices: selected, event_count: count })
}


pub fn wall_nodal_force_history(impulses: &[Vec<[f64; 3]>], t: &WallTransfer) -> CaeResult<Vec<Vec<f64>>> {
    if impulses.is_empty() || impulses.iter().any(|r| r.len() != t.event_count) {
        return Err(err("Matching time-by-event XYZ impulse history required"));
    }
    let scale = t.map.impulse_unit_n_s / t.map.dt_s;
    Ok(impulses
        .iter()
        .map(|row| {
            let selected: Vec<[f64; 3]> = t.event_indices.iter().map(|&i| row[i]).collect();
            link_to_nodes(&t.map.weights, &selected, t.map.nodes).into_iter().map(|v| v * scale).collect()
        })
        .collect())
}

#[derive(Clone, Debug)]
pub struct Expansion {
    pub coefficient: Vec<f64>,
    pub reference: Vec<f64>,
    pub weights: Vec<f64>,
    pub minimum_temperature_k: f64,
    pub maximum_temperature_k: f64,
    pub load_per_strain: Vec<Vec<f64>>,
    pub modulus: Vec<f64>,
    pub cells: usize,
}

impl Expansion {
    #[must_use]
    pub fn sample(&self, temperature: &[Vec<f64>]) -> (Vec<Vec<f64>>, Vec<Vec<f64>>) {
        let ne = self.coefficient.len();
        let sampled: Vec<Vec<f64>> = temperature
            .iter()
            .map(|t| {
                (0..ne)
                    .map(|e| (0..self.cells).map(|c| t[c] * self.weights[e * self.cells + c]).sum())
                    .collect()
            })
            .collect();
        let strain = sampled
            .iter()
            .map(|row: &Vec<f64>| {
                row.iter().enumerate().map(|(e, v)| (v - self.reference[e]) * self.coefficient[e]).collect()
            })
            .collect();
        (sampled, strain)
    }
}

#[derive(Clone, Debug)]
pub struct PorousStructure {
    pub config: Map<String, Value>,
    pub mapping: TransferMap,
    pub mech: Mechanics,
    pub receive: bool,
    pub expansion: Option<Expansion>,
    pub wall_transfer: Option<WallTransfer>,
    pub spacing_m: f64,
    pub maximum_strain: f64,
    pub maximum_motion_cells: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct GridInfo {
    pub shape: [usize; 3],
    pub origin_m: [f64; 3],
    pub spacing_m: f64,
    pub step_s: f64,
    pub drag_max_per_s: f64,
}

impl GridInfo {
    fn cell_positions(&self) -> Vec<f64> {
        let [nx, ny, nz] = self.shape;
        let mut out = Vec::with_capacity(3 * nx * ny * nz);
        for i in 0..nx {
            for j in 0..ny {
                for k in 0..nz {
                    for (a, v) in [i, j, k].into_iter().enumerate() {
                        out.push(self.origin_m[a] + (v as f64 + 0.5) * self.spacing_m);
                    }
                }
            }
        }
        out
    }
}

const REQUIRED: [&str; 13] = [
    "nodes_m",
    "tetrahedra",
    "density_kg_m3",
    "young_Pa",
    "poisson",
    "fixed_dofs",
    "trace_weights",
    "mass_damping_per_s",
    "stiffness_damping_s",
    "maximum_strain",
    "maximum_motion_cells",
    "history_byte_budget",
    "provenance",
];


#[allow(clippy::too_many_lines)]
pub fn prepare(
    config: &Value,
    grid: &GridInfo,
    density_unit: &Value,
    step_count: usize,
) -> CaeResult<PorousStructure> {
    let ok = config.as_object().is_some_and(|m| {
        REQUIRED.iter().all(|k| m.contains_key(*k))
            && m.keys().all(|k| {
                REQUIRED.contains(&k.as_str())
                    || ["wall_loads", "thermal_expansion", "receive_porous_reaction"].contains(&k.as_str())
            })
    });
    let Some(s) = config.as_object().filter(|_| ok) else {
        return Err(err("Porous structure requires the documented mesh, mapping, materials and limits"));
    };
    let nodes_a = asarray(&s["nodes_m"]);
    if nodes_a.shape.len() != 2 || nodes_a.shape[1] != 3 || !(4..=256).contains(&nodes_a.shape[0]) {
        return Err(err("Porous structure requires 4–256 nodes"));
    }
    if !s["provenance"].as_str().is_some_and(|v| !v.trim().is_empty()) {
        return Err(err("Structural material provenance required"));
    }
    let mut scalars = std::collections::BTreeMap::new();
    for key in ["mass_damping_per_s", "stiffness_damping_s", "maximum_strain", "maximum_motion_cells"] {
        let a = asarray(&s[key]);
        match a.real_scalar().filter(|v| v.is_finite() && a.kind != Kind::Bool) {
            Some(v) => scalars.insert(key, v),
            None => return Err(err(format!("Finite scalar {key} required"))),
        };
    }
    let (md, sd) = (scalars["mass_damping_per_s"], scalars["stiffness_damping_s"]);
    let (maximum_strain, maximum_motion) = (scalars["maximum_strain"], scalars["maximum_motion_cells"]);
    if md.min(sd) < 0.0
        || !(maximum_strain > 0.0 && maximum_strain <= 0.05)
        || !(maximum_motion > 0.0 && maximum_motion <= 0.01)
    {
        return Err(err("Nonnegative damping, strain <=0.05 and motion <=0.01 cells required"));
    }
    let nn = nodes_a.shape[0];
    let fixed = asarray(&s["fixed_dofs"]);
    if fixed.shape != nodes_a.shape || fixed.kind != Kind::Bool {
        return Err(err("Boolean node-by-XYZ fixed_dofs required"));
    }
    let free: Vec<usize> = fixed.bools().iter().enumerate().filter(|(_, f)| !**f).map(|(i, _)| i).collect();
    if free.is_empty() {
        return Err(err("At least one free structural DOF required"));
    }
    let cells = grid.shape.iter().product::<usize>();
    let receive = match s.get("receive_porous_reaction") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(err("receive_porous_reaction must be boolean")),
    };
    let (positions, weights) = if receive {
        (
            NdArray { shape: vec![cells, 3], kind: Kind::Float, data: grid.cell_positions() },
            asarray(&s["trace_weights"]),
        )
    } else {
        if grid.drag_max_per_s != 0.0 || !s["trace_weights"].is_null() {
            return Err(err("Wall-only mechanics requires drag_max_per_s=0 and trace_weights=null"));
        }
        if s.get("wall_loads").is_none_or(Value::is_null) {
            return Err(err("Wall-only mechanics requires explicit wall_loads"));
        }
        let mut w = vec![0.0; nn];
        w[0] = 1.0;
        (
            NdArray { shape: vec![1, 3], kind: nodes_a.kind, data: nodes_a.data[..3].to_vec() },
            NdArray { shape: vec![1, nn], kind: Kind::Float, data: w },
        )
    };
    let mapping =
        porous_transfer_map(&positions, &nodes_a, &weights, grid.spacing_m, grid.step_s, density_unit)?;
    let (_, tets, assembly, [_, young, poisson]) = assemble(config, &nodes_a)?;
    let ndof = 3 * nn;
    let mass = dense_sub(&assembly.mass.to_dense(), ndof, &free);
    let stiffness = dense_sub(&assembly.stiffness.to_dense(), ndof, &free);
    for (name, matrix) in [("mass", &mass), ("supported stiffness", &stiffness)] {
        if !well_conditioned(matrix)? {
            return Err(err(format!("{name} must be positive definite and well conditioned")));
        }
    }
    let nt = step_count + 1;
    let ne = assembly.volumes.len();
    let nf = free.len();
    let required =
        8 * (nt * (nn * 6 + ne * 6) + step_count * cells * 3 + nf * nf * 3 + ne * 6 * nn * 3 + cells * nn);
    let budget = py_int(&s["history_byte_budget"]);
    if budget.is_none_or(|b| b < i64::try_from(required).unwrap_or(i64::MAX)) {
        return Err(err(format!("Structural history needs at least {required} bytes, excluding AD")));
    }
    let damping = DenseMatrix {
        nrows: nf,
        ncols: nf,
        data: mass.data.iter().zip(&stiffness.data).map(|(m, k)| md * m + sd * k).collect(),
    };
    let stress = DenseMatrix { nrows: 6 * ne, ncols: ndof, data: assembly.stress.to_dense() };
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
    let mut out = PorousStructure {
        config: s.clone(),
        mapping,
        mech,
        receive,
        expansion: None,
        wall_transfer: None,
        spacing_m: grid.spacing_m,
        maximum_strain,
        maximum_motion_cells: maximum_motion,
    };
    if let Some(spec) = s.get("thermal_expansion").filter(|v| !v.is_null()) {
        let extra = 8 * (ne * cells + nt * ne * 8 + (step_count + ne) * nn * 3);
        if budget.is_none_or(|b| b < i64::try_from(required + extra).unwrap_or(i64::MAX)) {
            return Err(err("Structural history budget must also cover thermal expansion"));
        }
        out.expansion = Some(prepare_expansion(spec, &out, &nodes_a, &tets, grid)?);
    }
    Ok(out)
}


fn prepare_expansion(
    config: &Value,
    structure: &PorousStructure,
    nodes: &NdArray,
    tets: &[[usize; 4]],
    grid: &GridInfo,
) -> CaeResult<Expansion> {
    const KEYS: [&str; 5] = [
        "coefficient_per_K",
        "stress_free_temperature_K",
        "temperature_weights",
        "minimum_temperature_K",
        "maximum_temperature_K",
    ];
    let Some(c) = config.as_object().filter(|m| m.len() == 5 && KEYS.iter().all(|k| m.contains_key(*k)))
    else {
        return Err(err(
            "Thermal expansion requires coefficients, reference, temperature mapping and validity interval",
        ));
    };
    let mut limits = [0.0; 2];
    for (i, key) in ["minimum_temperature_K", "maximum_temperature_K"].iter().enumerate() {
        let a = asarray(&c[*key]);
        match a.real_scalar().filter(|v| v.is_finite() && a.kind != Kind::Bool) {
            Some(v) => limits[i] = v,
            None => return Err(err("Finite scalar thermal-expansion temperature limits required")),
        }
    }
    let [lo, hi] = limits;
    if !(0.0 < lo && lo < hi) {
        return Err(err("Positive ordered thermal-expansion temperature interval required"));
    }
    let mech = &structure.mech;
    let ne = mech.elements();
    let mut values = Vec::new();
    for key in ["coefficient_per_K", "stress_free_temperature_K"] {
        let v = asarray(&c[key]);
        if v.shape != [ne] || !v.is_real() || !v.all_finite() {
            return Err(err(
                "One finite expansion coefficient and reference temperature per element required",
            ));
        }
        values.push(v.data);
    }
    let (alpha, reference) = (values[0].clone(), values[1].clone());
    if reference.iter().any(|r| *r < lo || *r > hi) {
        return Err(err("Stress-free temperature outside expansion validity interval"));
    }
    if alpha
        .iter()
        .zip(&reference)
        .any(|(a, r)| a.abs() * (lo - r).abs().max((hi - r).abs()) * 3f64.sqrt() > structure.maximum_strain)
    {
        return Err(err("Thermal eigenstrain exceeds small-strain limit"));
    }
    let centres: Vec<f64> = tets
        .iter()
        .flat_map(|t| (0..3).map(move |a| (t, a)))
        .map(|(t, a)| (0..4).map(|k| nodes.data[3 * t[k] + a]).sum::<f64>() / 4.0)
        .collect();
    let cells = grid.shape.iter().product::<usize>();
    let centres = NdArray { shape: vec![ne, 3], kind: Kind::Float, data: centres };
    let cell_positions = NdArray { shape: vec![cells, 3], kind: Kind::Float, data: grid.cell_positions() };
    let weights = validate_projection(&centres, &cell_positions, &asarray(&c["temperature_weights"]))?;
    let ndof = 3 * mech.nodes;
    let load_per_strain = (0..ne)
        .map(|e| {
            (0..ndof)
                .map(|j| mech.volumes[e] * (0..3).map(|i| mech.stress.get(6 * e + i, j)).sum::<f64>())
                .collect()
        })
        .collect();
    Ok(Expansion {
        coefficient: alpha,
        reference,
        weights,
        minimum_temperature_k: lo,
        maximum_temperature_k: hi,
        load_per_strain,
        modulus: mech.young.iter().zip(&mech.poisson).map(|(e, nu)| e / (1.0 - 2.0 * nu)).collect(),
        cells,
    })
}


#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn tetrahedral_trace_weights(
    positions: &[[f64; 3]],
    nodes: &NdArray,
    tets: &NdArray,
) -> CaeResult<Vec<f64>> {
    let nn = nodes.shape[0];
    let np = positions.len();
    let mut weights = vec![0.0; np * nn];
    let mut unmapped = vec![true; np];
    for t in tets.data.chunks(4) {
        let ids: Vec<usize> = (0..np).filter(|&i| unmapped[i]).collect();
        if ids.is_empty() {
            break;
        }
        let tet = [t[0] as usize, t[1] as usize, t[2] as usize, t[3] as usize];
        let v: [[f64; 3]; 4] = std::array::from_fn(|k| std::array::from_fn(|a| nodes.data[3 * tet[k] + a]));

        let m = DenseMatrix {
            nrows: 3,
            ncols: 3,
            data: (0..9).map(|r| v[r % 3 + 1][r / 3] - v[0][r / 3]).collect(),
        };
        let rhs: Vec<f64> = (0..3)
            .flat_map(|a| ids.iter().map(move |&i| (i, a)))
            .map(|(i, a)| positions[i][a] - v[0][a])
            .collect();
        let local = implexity_linalg::dense::solve(&m, &rhs, ids.len()).map_err(|e| err(e.to_string()))?;
        for (col, &i) in ids.iter().enumerate() {
            let l: [f64; 3] = std::array::from_fn(|a| local[a * ids.len() + col]);
            let bary = [1.0 - (l[0] + l[1] + l[2]), l[0], l[1], l[2]];
            if bary.iter().all(|b| *b >= -1e-12 && *b <= 1.0 + 1e-12) {
                let clipped: [f64; 4] = bary.map(|b| b.max(0.0));
                let sum: f64 = clipped.iter().sum();
                for k in 0..4 {
                    weights[i * nn + tet[k]] = clipped[k] / sum;
                }
                unmapped[i] = false;
            }
        }
    }
    if unmapped.iter().any(|u| *u) {
        return Err(err(
            "Selected wall positions are outside the structural mesh; no extrapolated load mapping",
        ));
    }
    let positions = NdArray {
        shape: vec![np, 3],
        kind: Kind::Float,
        data: positions.iter().flatten().copied().collect(),
    };
    let weights = NdArray { shape: vec![np, nn], kind: Kind::Float, data: weights };
    validate_projection(&positions, nodes, &weights)
}


#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn attach_wall_loads(
    ps: &mut PorousStructure,
    grid: &GridInfo,
    transport: &mut Transport,
    density_unit: &Value,
    step_count: usize,
) -> CaeResult<()> {
    let Some(config) = ps.config.get("wall_loads").filter(|v| !v.is_null()).cloned() else {
        return Ok(());
    };
    let keys: Option<Vec<&str>> = config.as_object().map(|m| {
        let mut k: Vec<&str> = m.keys().map(String::as_str).collect();
        k.sort_unstable();
        k
    });
    let explicit = ["event_indices", "history_byte_budget", "maximum_events", "trace_weights"];
    let faces_form = ["faces", "history_byte_budget", "maximum_events"];
    let valid = keys.as_deref().is_some_and(|k| k == explicit || k == faces_form);
    if !valid {
        return Err(err(
            "Wall loads require budgets and either named faces or explicit event indices/weights",
        ));
    }
    let maximum = py_int(&config["maximum_events"])
        .filter(|v| *v >= 1)
        .map_or(0, |v| usize::try_from(v).unwrap_or(usize::MAX));
    let events = if matches!(transport.kind, TransportKind::Slab { .. }) {
        slab_wall_events(transport, maximum)?
    } else {
        box_wall_events(transport, maximum)?
    };
    let nodes = asarray(&ps.config["nodes_m"]);
    let (indices, weights) = if let Some(faces) = config.get("faces") {
        const NAMES: [&str; 6] = ["x-", "x+", "y-", "y+", "z-", "z+"];
        let list: Option<Vec<&str>> = faces.as_array().map(|a| a.iter().filter_map(Value::as_str).collect());
        let ok = list.as_ref().is_some_and(|l| {
            !l.is_empty()
                && l.len() == faces.as_array().map_or(0, Vec::len)
                && l.iter().all(|f| NAMES.contains(f))
                && l.iter().collect::<std::collections::BTreeSet<_>>().len() == l.len()
        });
        let Some(list) = list.filter(|_| ok) else {
            return Err(err("Unique nonempty face names x-/x+/y-/y+/z-/z+ required"));
        };
        let chosen: Vec<usize> = list.iter().filter_map(|f| NAMES.iter().position(|n| n == f)).collect();
        if chosen.iter().any(|f| transport.periodic[f / 2]) {
            return Err(err("Periodic faces cannot receive wall loads"));
        }
        let idx: Vec<usize> =
            (0..events.face_indices.len()).filter(|&i| chosen.contains(&events.face_indices[i])).collect();
        let positions: Vec<[f64; 3]> = idx
            .iter()
            .map(|&i| {
                std::array::from_fn(|a| {
                    grid.origin_m[a] + (events.positions_lattice[i][a] + 0.5) * grid.spacing_m
                })
            })
            .collect();
        let w = tetrahedral_trace_weights(&positions, &nodes, &asarray(&ps.config["tetrahedra"]))?;
        (
            NdArray {
                shape: vec![idx.len()],
                kind: Kind::Int,
                data: idx.iter().map(|&i| i as f64).collect(),
            },
            NdArray { shape: vec![idx.len(), nodes.shape[0]], kind: Kind::Float, data: w },
        )
    } else {
        (asarray(&config["event_indices"]), asarray(&config["trace_weights"]))
    };
    let transfer = wall_transfer_map(
        &events,
        &indices,
        &nodes,
        &weights,
        grid.origin_m,
        grid.spacing_m,
        grid.step_s,
        density_unit,
    )?;
    let ne = events.face_indices.len();
    let needed =
        8 * (step_count * ne * 3 + ne * 9 + transfer.map.weights.len() + step_count * nodes.shape[0] * 3);
    let budget = py_int(&config["history_byte_budget"]);
    if budget.is_none_or(|b| b < i64::try_from(needed).unwrap_or(i64::MAX)) {
        return Err(err(format!("Wall event history/mapping need at least {needed} bytes, excluding AD")));
    }
    transport.wall_events = Some(events);
    ps.wall_transfer = Some(transfer);
    Ok(())
}

#[derive(Clone, Debug)]
pub struct StructureHistory {
    pub motion: Motion,
    pub time_s: Vec<f64>,
    pub nodal_force_n: Vec<Vec<f64>>,
    pub element_temperature_k: Option<Vec<Vec<f64>>>,
    pub thermal_eigenstrain: Option<Vec<Vec<f64>>>,
}


pub fn history(
    ps: &PorousStructure,
    impulses: &[Vec<[f64; 3]>],
    wall_impulses: Option<&[Vec<[f64; 3]>]>,
    solid_temperature: Option<&[Vec<f64>]>,
) -> CaeResult<StructureHistory> {
    let steps = impulses.len();
    if steps < 1 {
        return Err(err("Nonempty time/cell/XYZ impulse history required"));
    }
    let nn = ps.mech.nodes;
    let m = &ps.mapping;
    let rows = if ps.receive { impulses[0].len() } else { 1 };
    if rows != m.rows() {
        return Err(err("Impulse history cells must match the projection"));
    }
    let scale = m.impulse_unit_n_s / m.dt_s;
    let mut nodal: Vec<Vec<f64>> = if ps.receive {
        impulses
            .iter()
            .map(|row| link_to_nodes(&m.weights, row, nn).into_iter().map(|v| v * scale).collect())
            .collect()
    } else {
        vec![vec![0.0; 3 * nn]; steps]
    };
    if let Some(t) = &ps.wall_transfer {
        let Some(w) = wall_impulses else {
            return Err(err("Selected wall loading requires its actual impulse history"));
        };
        for (row, extra) in nodal.iter_mut().zip(wall_nodal_force_history(w, t)?) {
            for (a, b) in row.iter_mut().zip(extra) {
                *a += b;
            }
        }
    }
    let mut thermal_stress = None;
    let (mut element_temperature, mut eigenstrain) = (None, None);
    if let Some(x) = &ps.expansion {
        let Some(temperature) = solid_temperature else {
            return Err(err("Thermal expansion requires actual solid-temperature history"));
        };
        if temperature.len() < 2 || temperature.iter().any(|t| t.len() != x.cells) {
            return Err(err("Matching initial-plus-endpoint solid-temperature grids required"));
        }
        let (sampled, strain) = x.sample(temperature);
        for (n, row) in nodal.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v += (0..strain[n].len()).map(|e| strain[n][e] * x.load_per_strain[e][j]).sum::<f64>();
            }
        }
        thermal_stress = Some(
            strain
                .iter()
                .map(|row| row.iter().zip(&x.modulus).map(|(s, m)| s * m).collect::<Vec<f64>>())
                .collect::<Vec<_>>(),
        );
        element_temperature = Some(sampled);
        eigenstrain = Some(strain);
    }
    let times: Vec<f64> = (0..=steps).map(|n| n as f64 * m.dt_s).collect();
    let motion = ps.mech.respond(&nodal, &times, thermal_stress.as_deref())?;
    Ok(StructureHistory {
        motion,
        time_s: times,
        nodal_force_n: nodal,
        element_temperature_k: element_temperature,
        thermal_eigenstrain: eigenstrain,
    })
}


pub fn admit(h: &StructureHistory, ps: &PorousStructure) -> CaeResult<Map<String, Value>> {
    let finite_rows =
        |v: &Option<Vec<Vec<f64>>>| v.as_ref().is_none_or(|r| r.iter().flatten().all(|x| x.is_finite()));
    if !h.motion.is_finite()
        || !h.nodal_force_n.iter().flatten().all(|v| v.is_finite())
        || !finite_rows(&h.element_temperature_k)
        || !finite_rows(&h.thermal_eigenstrain)
    {
        return Err(err("Nonfinite porous structural history"));
    }
    let (strain, _) = ps.mech.strain_measures(&h.motion.stress_physical_pa, None);
    let mut total = strain;
    if let (Some(x), Some(t), Some(eig)) = (&ps.expansion, &h.element_temperature_k, &h.thermal_eigenstrain) {
        if t.iter()
            .flatten()
            .any(|v| !v.is_finite() || *v < x.minimum_temperature_k || *v > x.maximum_temperature_k)
        {
            return Err(err("Solid temperature outside thermal-expansion validity interval"));
        }
        total = ps.mech.strain_measures(&h.motion.stress_physical_pa, Some(eig)).1;
    }
    let motion = ps.mech.motion(&h.motion) / ps.spacing_m;
    if strain.max(total) > ps.maximum_strain || motion > ps.maximum_motion_cells {
        return Err(err("Porous receiver exceeds linear strain or fixed-medium motion limit"));
    }
    let error = h.motion.energy_error();
    if error > 1e-8 {
        return Err(err("Porous structural mechanical energy balance failed"));
    }
    let mut out = Map::new();
    out.insert("maximum_strain".into(), json!(strain));
    out.insert("maximum_total_strain".into(), json!(total));
    out.insert("maximum_motion_cells".into(), json!(motion));
    out.insert("mechanical_energy_relative_error".into(), json!(error));
    Ok(out)
}

pub type StructureBars = (Vec<Vec<[f64; 3]>>, Option<Vec<Vec<[f64; 3]>>>, Option<Vec<Vec<f64>>>);


pub fn stress_response_bar(
    ps: &PorousStructure,
    h: &StructureHistory,
    cells: usize,
) -> CaeResult<StructureBars> {
    let nt = h.time_s.len();
    let steps = nt - 1;
    let nn = ps.mech.nodes;
    let stress_bar = ps.mech.mean_squared_stress_bar(&h.motion.stress_physical_pa);
    let (nodal_bar, thermal_bar) =
        ps.mech.reverse(&h.time_s, vec![vec![0.0; 3 * nn]; nt], Some(&stress_bar))?;
    let m = &ps.mapping;
    let scale = m.impulse_unit_n_s / m.dt_s;
    let impulse_bar: Vec<Vec<[f64; 3]>> = nodal_bar
        .iter()
        .map(|nb| {
            if ps.receive {
                nodes_to_links(&m.weights, nb, cells, nn).into_iter().map(|v| v.map(|x| x * scale)).collect()
            } else {
                vec![[0.0; 3]; cells]
            }
        })
        .collect();
    let wall_bar = ps.wall_transfer.as_ref().map(|t| {
        let s = t.map.impulse_unit_n_s / t.map.dt_s;
        nodal_bar
            .iter()
            .map(|nb| {
                let selected = nodes_to_links(&t.map.weights, nb, t.event_indices.len(), t.map.nodes);
                let mut row = vec![[0.0; 3]; t.event_count];
                for (k, &i) in t.event_indices.iter().enumerate() {
                    row[i] = selected[k].map(|x| x * s);
                }
                row
            })
            .collect()
    });
    let temperature_bar = ps.expansion.as_ref().map(|x| {
        let ne = ps.mech.elements();
        (0..nt)
            .map(|t| {
                let mut out = vec![0.0; x.cells];
                for e in 0..ne {
                    let mut strain_bar = thermal_bar[t][e] * x.modulus[e];
                    if t < steps {
                        strain_bar +=
                            x.load_per_strain[e].iter().zip(&nodal_bar[t]).map(|(l, b)| l * b).sum::<f64>();
                    }
                    let g = strain_bar * x.coefficient[e];
                    for c in 0..x.cells {
                        out[c] += g * x.weights[e * x.cells + c];
                    }
                }
                out
            })
            .collect()
    });
    Ok((impulse_bar, wall_bar, temperature_bar))
}
