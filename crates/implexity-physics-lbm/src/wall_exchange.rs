// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::{CaeError, CaeResult};

use crate::d3q19::{C, OPPOSITE, Q, W};
use crate::nparray::NdArray;
use crate::solver::{LbmProblem, Tau, collide};

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub cells: Vec<usize>,
    pub links: Vec<usize>,
    pub positions: Vec<[f64; 3]>,
}

#[must_use]
pub fn layout(p: &LbmProblem) -> Layout {
    let grid = p.grid;
    let blocked = p.streaming.blocked();
    let mut cells = Vec::new();
    let mut links = Vec::new();
    let mut positions = Vec::new();
    for x in 0..grid.cells() {
        if p.solid_mask[x] {
            continue;
        }
        for q in 0..Q {
            if !blocked[x * Q + q] {
                continue;
            }
            if p.ports.iter().any(|port| port.port.mask[x] && C[q][port.port.axis] == port.port.sign) {
                continue;
            }
            let ijk = grid.coords(x);
            cells.push(x);
            links.push(q);
            positions.push(std::array::from_fn(|a| {
                p.origin_m[a] + (ijk[a] as f64 + 0.5 - 0.5 * f64::from(C[q][a])) * p.spacing_m
            }));
        }
    }
    Layout { cells, links, positions }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Loads {
    pub positions_m: Vec<[f64; 3]>,
    pub force_n: Vec<[f64; 3]>,
    pub lattice_momentum_exchange_force_n: Vec<[f64; 3]>,
}

#[must_use]
pub fn loads(p: &LbmProblem, alpha: &[f64], before: &[f64], pressure_datum_pa: f64, tau: Tau<'_>) -> Loads {
    let mut post = vec![0.0; before.len()];
    collide(before, alpha, p.acceleration_lattice(), tau, &mut post);
    loads_from_post(p, &post, pressure_datum_pa)
}

#[must_use]
pub fn loads_from_post(p: &LbmProblem, post: &[f64], pressure_datum_pa: f64) -> Loads {
    let l = layout(p);
    let scale = p.density_kg_m3 * p.spacing_m.powi(4) / (p.step_s * p.step_s);
    let unit = p.density_kg_m3 * (p.spacing_m / p.step_s).powi(2) / 3.0;
    let mut force = Vec::with_capacity(l.cells.len());
    let mut raw = Vec::with_capacity(l.cells.len());
    for (&cell, &link) in l.cells.iter().zip(&l.links) {
        let outgoing = post[cell * Q + OPPOSITE[link]];
        let base = W[link];
        let magnitude = outgoing - base + base * pressure_datum_pa / unit;
        force.push(std::array::from_fn(|a| -2.0 * f64::from(C[link][a]) * magnitude * scale));
        raw.push(std::array::from_fn(|a| -2.0 * f64::from(C[link][a]) * outgoing * scale));
    }
    Loads { positions_m: l.positions, force_n: force, lattice_momentum_exchange_force_n: raw }
}

#[must_use]
pub fn loads_from_post_vjp(p: &LbmProblem, layout: &Layout, force_bar: &[[f64; 3]]) -> Vec<f64> {
    let scale = p.density_kg_m3 * p.spacing_m.powi(4) / (p.step_s * p.step_s);
    let mut post_bar = vec![0.0; p.cells() * Q];
    for ((&cell, &link), fb) in layout.cells.iter().zip(&layout.links).zip(force_bar) {
        let mut g = 0.0;
        for a in 0..3 {
            g += -2.0 * f64::from(C[link][a]) * fb[a] * scale;
        }
        post_bar[cell * Q + OPPOSITE[link]] += g;
    }
    post_bar
}


pub fn validate_projection(
    link_positions: &NdArray,
    nodes: &NdArray,
    weights: &NdArray,
) -> CaeResult<Vec<f64>> {
    for (name, a) in [("link positions", link_positions), ("solid nodes", nodes), ("trace weights", weights)]
    {
        if !a.is_real() || !a.all_finite() {
            return Err(CaeError::contract(format!("{name} must contain finite real values")));
        }
    }
    if link_positions.shape.len() != 2
        || link_positions.shape[1] != 3
        || nodes.shape.len() != 2
        || nodes.shape[1] != 3
    {
        return Err(CaeError::contract("Interface and solid positions require XYZ rows"));
    }
    let (nl, nn) = (link_positions.shape[0], nodes.shape[0]);
    if weights.shape != [nl, nn] || weights.data.iter().any(|w| *w < 0.0) {
        return Err(CaeError::contract("Nonnegative link-by-node interpolation matrix required"));
    }
    for r in 0..nl {
        let s: f64 = weights.data[r * nn..(r + 1) * nn].iter().sum();
        if (s - 1.0).abs() > 1e-12 {
            return Err(CaeError::contract("Interface interpolation must preserve constants"));
        }
    }
    let max_abs = |a: &NdArray| a.data.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let scale = max_abs(nodes).max(max_abs(link_positions)).max(1e-12);
    for r in 0..nl {
        for a in 0..3 {
            let mut v = 0.0;
            for k in 0..nn {
                v += weights.data[r * nn + k] * nodes.data[k * 3 + a];
            }
            if (v - link_positions.data[r * 3 + a]).abs() > 1e-10 * scale {
                return Err(CaeError::contract(
                    "Interface interpolation must reproduce positions to preserve moment",
                ));
            }
        }
    }
    Ok(weights.data.clone())
}

#[must_use]
pub fn nodal_forces(force: &[[f64; 3]], weights: &[f64], nodes: usize) -> Vec<[f64; 3]> {
    let mut out = vec![[0.0; 3]; nodes];
    for (r, f) in force.iter().enumerate() {
        for (k, o) in out.iter_mut().enumerate() {
            let w = weights[r * nodes + k];
            for a in 0..3 {
                o[a] += w * f[a];
            }
        }
    }
    out
}

#[must_use]
pub fn interface_velocity(velocity: &[[f64; 3]], weights: &[f64], links: usize) -> Vec<[f64; 3]> {
    let nodes = velocity.len();
    (0..links)
        .map(|r| {
            let mut v = [0.0; 3];
            for (k, n) in velocity.iter().enumerate() {
                for a in 0..3 {
                    v[a] += weights[r * nodes + k] * n[a];
                }
            }
            v
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadHistory {
    pub positions_m: Vec<[f64; 3]>,
    pub interval_force_n: Vec<Vec<[f64; 3]>>,
    pub time_s: Vec<f64>,
}


pub fn load_history(
    p: &LbmProblem,
    alpha: &[f64],
    populations: &[Vec<f64>],
    pressure_datum_pa: f64,
    tau_history: Option<&[Vec<f64>]>,
) -> CaeResult<LoadHistory> {
    if populations.len() != p.step_count + 1 || populations.iter().any(|s| s.len() != p.cells() * Q) {
        return Err(CaeError::contract("Wall history requires all scheduled population states"));
    }
    if let Some(t) = tau_history
        && (t.len() != p.step_count || t.iter().any(|v| v.len() != p.cells()))
    {
        return Err(CaeError::contract("Wall relaxation history must match interval start temperatures"));
    }
    let mut forces = Vec::with_capacity(p.step_count);
    for (n, before) in populations[..p.step_count].iter().enumerate() {
        let tau = tau_history.map_or(Tau::Uniform(p.tau), |t| Tau::PerCell(&t[n]));
        forces.push(loads(p, alpha, before, pressure_datum_pa, tau).force_n);
    }
    Ok(LoadHistory {
        positions_m: layout(p).positions,
        interval_force_n: forces,
        time_s: (0..=p.step_count).map(|n| n as f64 * p.step_s).collect(),
    })
}
