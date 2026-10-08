// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::dense::{DenseMatrix, cond, matrix_rank, pinv};
use serde_json::{Map, Value};

use crate::d3q19::{C, Grid, OPPOSITE, Q, W, c};
use crate::nparray::asarray;

pub type Cell<S> = [S; Q];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconstruction {
    LegacyMoment,
    RegularizedKnownStress,
    NeighborNonEquilibrium,
}

impl Reconstruction {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LegacyMoment => "legacy_moment",
            Self::RegularizedKnownStress => "regularized_known_stress",
            Self::NeighborNonEquilibrium => "neighbor_non_equilibrium",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Port {
    pub id: String,
    pub pressure: bool,
    pub face: String,
    pub mask: Vec<bool>,
    pub cells: Vec<usize>,
    pub axis: usize,
    pub sign: i32,
    pub velocity_lattice: [f64; 3],
    pub density_lattice: Option<f64>,
    pub reconstruction: Reconstruction,
    pub authored: Map<String, Value>,
}

impl Port {
    #[must_use]
    pub fn sources(&self, grid: &Grid) -> Vec<usize> {
        if self.reconstruction != Reconstruction::NeighborNonEquilibrium {
            return self.cells.clone();
        }
        let mut offset = [0i64; 3];
        offset[self.axis] = i64::from(self.sign);
        self.cells.iter().map(|&x| grid.wrap(x, offset)).collect()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PortContext<'a> {
    pub grid: Grid,
    pub periodic: [bool; 3],
    pub solid: &'a [bool],
    pub design_region: &'a [bool],
    pub fixed_design: &'a [f64],
    pub acceleration_m_s2: [f64; 3],
    pub step_s: f64,
    pub spacing_m: f64,
    pub mach_limit: f64,
    pub density_kg_m3: f64,
}

const FACES: [&str; 6] = ["xmin", "xmax", "ymin", "ymax", "zmin", "zmax"];

fn err(message: &str) -> CaeError {
    CaeError::contract(message)
}


#[allow(clippy::too_many_lines)]
pub fn normalize_ports(ports: &Value, ctx: &PortContext<'_>) -> CaeResult<Vec<Port>> {
    let Some(list) = ports.as_array() else {
        return Err(err("ports must be a list"));
    };
    let grid = ctx.grid;
    let n = grid.cells();
    let accelerated = ctx.acceleration_m_s2.iter().any(|a| *a != 0.0);
    let mut used = vec![false; n];
    let mut ids = std::collections::BTreeSet::new();
    let mut out: Vec<Port> = Vec::new();
    for port in list {
        let kind = port.get("kind").and_then(Value::as_str);
        let Some(map) = port.as_object().filter(|_| matches!(kind, Some("velocity" | "pressure"))) else {
            return Err(err("Port kind must be velocity or pressure"));
        };
        let pressure = kind == Some("pressure");
        let mut required = vec!["id", "kind", "face", "mask", "velocity_m_s"];
        if pressure {
            required.push("gauge_pressure_Pa");
        }
        let keys: Vec<&str> = map.keys().map(String::as_str).filter(|k| *k != "reconstruction").collect();
        let exact = keys.len() == required.len() && required.iter().all(|r| keys.contains(r));
        let id = map.get("id").and_then(Value::as_str);
        if !exact || id.is_none_or(|s| s.trim().is_empty() || ids.contains(s)) {
            return Err(err("Unique port id and exact port fields required"));
        }
        let id = id.unwrap_or_default().to_string();
        let reconstruction = match map.get("reconstruction") {
            None => Reconstruction::LegacyMoment,
            Some(Value::String(s)) if s == "legacy_moment" => Reconstruction::LegacyMoment,
            Some(Value::String(s)) if s == "regularized_known_stress" => {
                Reconstruction::RegularizedKnownStress
            }
            Some(Value::String(s)) if s == "neighbor_non_equilibrium" => {
                Reconstruction::NeighborNonEquilibrium
            }
            Some(_) => {
                return Err(err(
                    "Port reconstruction must be legacy_moment, regularized_known_stress or neighbor_non_equilibrium",
                ));
            }
        };
        if reconstruction == Reconstruction::NeighborNonEquilibrium && (!pressure || accelerated) {
            return Err(err(
                "neighbor_non_equilibrium currently requires a pressure port and zero acceleration",
            ));
        }
        if reconstruction == Reconstruction::RegularizedKnownStress && accelerated {
            return Err(err(
                "regularized_known_stress ports require zero acceleration; forcing-consistent stress reconstruction is not implemented",
            ));
        }
        ids.insert(id.clone());
        let face = map.get("face").and_then(Value::as_str).filter(|f| FACES.contains(f));
        let Some(face) = face else {
            return Err(err("Port must identify one Cartesian face"));
        };
        let axis = match face.as_bytes()[0] {
            b'x' => 0,
            b'y' => 1,
            _ => 2,
        };
        let sign: i32 = if face.ends_with("min") { 1 } else { -1 };
        if ctx.periodic[axis] {
            return Err(err("A periodic face cannot be a port"));
        }
        let mask_array = asarray(map.get("mask").unwrap_or(&Value::Null));
        if mask_array.shape != grid.shape
            || !mask_array.is_bool()
            || !mask_array.data.iter().any(|v| *v != 0.0)
        {
            return Err(err("Nonempty shape-matched boolean port mask required"));
        }
        let mask = mask_array.bools();
        let face_index = if sign == 1 { 0 } else { grid.shape[axis] - 1 };
        if (0..n).any(|x| mask[x] && (grid.coords(x)[axis] != face_index || used[x])) {
            return Err(err("Port mask must lie on its face and cannot overlap another port"));
        }
        for a in 0..3 {
            if a != axis
                && !ctx.periodic[a]
                && (0..n).any(|x| {
                    let i = grid.coords(x)[a];
                    mask[x] && (i == 0 || i == grid.shape[a] - 1)
                })
            {
                return Err(err(
                    "Port corners/edges with other closed faces require an unsupported corner closure",
                ));
            }
        }
        if (0..n).any(|x| mask[x] && (ctx.solid[x] || ctx.design_region[x] || ctx.fixed_design[x] != 1.0)) {
            return Err(err("Port cells must be protected, unobstructed, fixed-design fluid"));
        }
        let cells: Vec<usize> = (0..n).filter(|x| mask[*x]).collect();
        if reconstruction == Reconstruction::NeighborNonEquilibrium {
            let mut neighbors = Vec::with_capacity(cells.len());
            for &x in &cells {
                let mut coords = grid.coords(x);
                let moved = if sign > 0 { coords[axis].checked_add(1) } else { coords[axis].checked_sub(1) };
                match moved.filter(|v| *v < grid.shape[axis]) {
                    Some(v) => coords[axis] = v,
                    None => {
                        return Err(err("Neighbor pressure reconstruction requires an inward fluid plane"));
                    }
                }
                neighbors.push(grid.index(coords[0], coords[1], coords[2]));
            }
            if neighbors.iter().any(|&y| ctx.solid[y] || ctx.design_region[y] || ctx.fixed_design[y] != 1.0) {
                return Err(err(
                    "Neighbor pressure reconstruction requires protected, unobstructed, fixed-design inward fluid cells",
                ));
            }
        }
        let velocity = asarray(map.get("velocity_m_s").unwrap_or(&Value::Null));
        if velocity.shape != [3] || !velocity.is_real() || !velocity.all_finite() {
            return Err(err("Port velocity requires three finite components"));
        }
        let u: [f64; 3] = std::array::from_fn(|a| velocity.data[a] * ctx.step_s / ctx.spacing_m);
        if pressure && velocity.data[axis] != 0.0 {
            return Err(err(
                "Pressure port velocity specifies tangential components only; normal component must be zero",
            ));
        }
        if (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt() * 3f64.sqrt() > ctx.mach_limit {
            return Err(err("Authored port velocity exceeds Mach limit"));
        }
        let mut density = None;
        if pressure {
            let Some(gauge) = asarray(map.get("gauge_pressure_Pa").unwrap_or(&Value::Null)).real_scalar()
            else {
                return Err(err("Finite scalar gauge pressure required"));
            };
            let unit = ctx.spacing_m / ctx.step_s;
            let rho = 1.0 + gauge * 3.0 / (ctx.density_kg_m3 * (unit * unit));
            if !(0.9..=1.1).contains(&rho) {
                return Err(err("Pressure port density must remain within 10% of reference"));
            }
            density = Some(rho);
        }
        for &x in &cells {
            used[x] = true;
        }
        out.push(Port {
            id,
            pressure,
            face: face.to_string(),
            mask,
            cells,
            axis,
            sign,
            velocity_lattice: u,
            density_lattice: density,
            reconstruction,
            authored: map.clone(),
        });
    }
    for port in &out {
        if port.reconstruction == Reconstruction::NeighborNonEquilibrium
            && port.sources(&grid).iter().any(|&y| used[y])
        {
            return Err(err("Neighbor pressure reconstruction cannot sample another port"));
        }
    }
    Ok(out)
}

#[inline]
pub fn equilibrium<S: Scalar>(rho: S, u: [S; 3]) -> Cell<S> {
    let usq = u[0] * u[0] + u[1] * u[1] + u[2] * u[2];
    std::array::from_fn(|q| {
        let cu = dot_c(q, u);
        rho * W[q] * (S::one() + cu * 3.0 + cu * cu * 4.5 - usq * 1.5)
    })
}

#[inline]
pub fn dot_c<S: Scalar>(q: usize, u: [S; 3]) -> S {
    let mut acc: Option<S> = None;
    for (d, ud) in u.iter().enumerate() {
        let term = match C[q][d] {
            0 => continue,
            1 => *ud,
            _ => -*ud,
        };
        acc = Some(acc.map_or(term, |a| a + term));
    }
    acc.unwrap_or_else(S::zero)
}

#[inline]
pub fn moments<S: Scalar>(f: &Cell<S>) -> (S, [S; 3]) {
    let mut rho = f[0];
    for v in &f[1..] {
        rho += *v;
    }
    let mut m = [S::zero(); 3];
    for (d, md) in m.iter_mut().enumerate() {
        let mut acc = S::zero();
        for q in 0..Q {
            match C[q][d] {
                0 => {}
                1 => acc += f[q],
                _ => acc -= f[q],
            }
        }
        *md = acc;
    }
    (rho, m)
}

fn partition(axis: usize, sign: i32) -> (Vec<usize>, Vec<usize>, Vec<usize>) {
    let unknown = (0..Q).filter(|&q| C[q][axis] == sign).collect();
    let outgoing = (0..Q).filter(|&q| C[q][axis] == -sign).collect();
    let zero = (0..Q).filter(|&q| C[q][axis] == 0).collect();
    (unknown, outgoing, zero)
}

fn right_inverse(unknown: &[usize], tangent: [usize; 2]) -> Vec<[f64; 3]> {
    let rows: [Vec<f64>; 3] = [
        vec![1.0; unknown.len()],
        unknown.iter().map(|&q| c(q, tangent[0])).collect(),
        unknown.iter().map(|&q| c(q, tangent[1])).collect(),
    ];
    let mut m = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = rows[i].iter().zip(&rows[j]).map(|(a, b)| a * b).sum();
        }
    }
    let inv = implexity_geometry::linalg3::inv(&m).unwrap_or([[0.0; 3]; 3]);
    (0..unknown.len())
        .map(|k| std::array::from_fn(|j| (0..3).map(|i| rows[i][k] * inv[i][j]).sum()))
        .collect()
}

pub fn reconstruct<S: Scalar>(
    f: &Cell<S>,
    axis: usize,
    sign: i32,
    velocity_lattice: [f64; 3],
    acceleration_lattice: [f64; 3],
    density_lattice: Option<f64>,
) -> Cell<S> {
    let (unknown, outgoing, zero) = partition(axis, sign);
    let tangent: Vec<usize> = (0..3).filter(|a| *a != axis).collect();
    let tangent = [tangent[0], tangent[1]];
    let v0: [f64; 3] = std::array::from_fn(|d| velocity_lattice[d] - 0.5 * acceleration_lattice[d]);
    let mut compatibility = S::zero();
    let mut first = true;
    for &q in &zero {
        compatibility = if first { f[q] } else { compatibility + f[q] };
        first = false;
    }
    let mut out_sum = S::zero();
    let mut first = true;
    for &q in &outgoing {
        out_sum = if first { f[q] } else { out_sum + f[q] };
        first = false;
    }
    compatibility += out_sum * 2.0;
    let sgn = f64::from(sign);
    let (rho, v): (S, [S; 3]) = match density_lattice {
        None => (compatibility / (1.0 - sgn * v0[axis]), v0.map(S::from_f64)),
        Some(d) => {
            let rho = S::from_f64(d);
            let mut v = v0.map(S::from_f64);
            v[axis] = (S::one() - compatibility / rho) * sgn;
            (rho, v)
        }
    };
    let proposal: Vec<S> =
        unknown.iter().map(|&q| f[OPPOSITE[q]] + rho * (6.0 * W[q]) * dot_c(q, v)).collect();
    let mut known = *f;
    for &q in &unknown {
        known[q] = S::zero();
    }
    let (known_mass, known_momentum) = moments(&known);
    let remaining_mass = rho - known_mass;
    let target = [
        remaining_mass,
        rho * v[tangent[0]] - known_momentum[tangent[0]],
        rho * v[tangent[1]] - known_momentum[tangent[1]],
    ];

    let mut pa = [S::zero(); 3];
    for (k, &q) in unknown.iter().enumerate() {
        pa[0] += proposal[k];
        pa[1] += proposal[k] * c(q, tangent[0]);
        pa[2] += proposal[k] * c(q, tangent[1]);
    }
    let residual = [target[0] - pa[0], target[1] - pa[1], target[2] - pa[2]];
    let ri = right_inverse(&unknown, tangent);
    let mut out = *f;
    for (k, &q) in unknown.iter().enumerate() {
        let correction = residual[0] * ri[k][0] + residual[1] * ri[k][1] + residual[2] * ri[k][2];
        out[q] = proposal[k] + correction;
    }
    out
}


pub fn known_stress_operator(
    axis: usize,
    sign: i32,
) -> CaeResult<(Vec<usize>, Vec<[f64; 14]>, [[f64; 6]; Q])> {
    if axis > 2 || (sign != 1 && sign != -1) {
        return Err(err("Regularized port requires a Cartesian axis and sign"));
    }
    let basis: [[f64; 6]; Q] = std::array::from_fn(|q| {
        let qq = |a: usize, b: usize| c(q, a) * c(q, b) - if a == b { 1.0 / 3.0 } else { 0.0 };
        let entries = [qq(0, 0), qq(1, 1), qq(2, 2), 2.0 * qq(0, 1), 2.0 * qq(0, 2), 2.0 * qq(1, 2)];
        entries.map(|e| 4.5 * W[q] * e)
    });
    let known: Vec<usize> = (0..Q).filter(|&q| C[q][axis] != sign).collect();
    let mut weighted = Vec::with_capacity(known.len() * 6);
    for &q in &known {
        for e in basis[q] {
            weighted.push(e / W[q].sqrt());
        }
    }
    let matrix = DenseMatrix::new(known.len(), 6, weighted).map_err(|e| err(&e.to_string()))?;
    let rank = matrix_rank(&matrix, None).map_err(|e| err(&e.to_string()))?;
    let condition = cond(&matrix).map_err(|e| err(&e.to_string()))?;
    if rank != 6 || condition > 1e12 {
        return Err(err("Regularized port stress fit is rank deficient or ill conditioned"));
    }
    let p = pinv(&matrix, None).map_err(|e| err(&e.to_string()))?;
    let operator: Vec<[f64; 14]> =
        (0..6).map(|r| std::array::from_fn(|k| p.get(r, k) / W[known[k]].sqrt())).collect();
    Ok((known, operator, basis))
}

#[derive(Clone, Debug)]
pub struct StressOperator {
    known: Vec<usize>,
    operator: Vec<[f64; 14]>,
    basis: [[f64; 6]; Q],
}

impl StressOperator {

    pub fn new(axis: usize, sign: i32) -> CaeResult<Self> {
        let (known, operator, basis) = known_stress_operator(axis, sign)?;
        Ok(Self { known, operator, basis })
    }
}

pub fn reconstruct_regularized<S: Scalar>(
    f: &Cell<S>,
    axis: usize,
    sign: i32,
    velocity_lattice: [f64; 3],
    density_lattice: Option<f64>,
    op: &StressOperator,
) -> Cell<S> {
    let target = reconstruct(f, axis, sign, velocity_lattice, [0.0; 3], density_lattice);
    let (rho, m) = moments(&target);
    let velocity = [m[0] / rho, m[1] / rho, m[2] / rho];
    let eq = equilibrium(rho, velocity);
    let mut stress = [S::zero(); 6];
    for (r, s) in stress.iter_mut().enumerate() {
        let mut acc = S::zero();
        for (k, &q) in op.known.iter().enumerate() {
            acc += (f[q] - eq[q]) * op.operator[r][k];
        }
        *s = acc;
    }
    std::array::from_fn(|q| {
        let mut acc = S::zero();
        for (r, s) in stress.iter().enumerate() {
            acc += *s * op.basis[q][r];
        }
        eq[q] + acc
    })
}

pub fn reconstruct_neighbor<S: Scalar>(
    neighbor: &Cell<S>,
    axis: usize,
    velocity_lattice: [f64; 3],
    density_lattice: f64,
) -> Cell<S> {
    let (rho, m) = moments(neighbor);
    let velocity = [m[0] / rho, m[1] / rho, m[2] / rho];
    let mut target = velocity_lattice.map(S::from_f64);
    target[axis] = velocity[axis];
    let boundary = equilibrium(S::from_f64(density_lattice), target);
    let local = equilibrium(rho, velocity);
    std::array::from_fn(|q| boundary[q] + neighbor[q] - local[q])
}

#[derive(Clone, Debug)]
pub struct CompiledPort {
    pub port: Port,
    pub sources: Vec<usize>,
    stress: Option<StressOperator>,
    linear: Option<Vec<f64>>,
}

impl CompiledPort {

    pub fn new(port: Port, grid: &Grid, acceleration_lattice: [f64; 3]) -> CaeResult<Self> {
        let sources = port.sources(grid);
        let stress = match port.reconstruction {
            Reconstruction::RegularizedKnownStress => Some(StressOperator::new(port.axis, port.sign)?),
            _ => None,
        };
        let mut compiled = Self { port, sources, stress, linear: None };
        if compiled.port.reconstruction == Reconstruction::LegacyMoment {
            let reference: Vec<f64> = W.to_vec();
            compiled.linear = Some(compiled.cell_jacobian(&reference, acceleration_lattice)?);
        }
        Ok(compiled)
    }

    pub fn apply_cell<S: Scalar>(&self, source: &Cell<S>, acceleration_lattice: [f64; 3]) -> Cell<S> {
        let p = &self.port;
        match (p.reconstruction, &self.stress) {
            (Reconstruction::NeighborNonEquilibrium, _) => {
                reconstruct_neighbor(source, p.axis, p.velocity_lattice, p.density_lattice.unwrap_or(1.0))
            }
            (Reconstruction::RegularizedKnownStress, Some(op)) => {
                reconstruct_regularized(source, p.axis, p.sign, p.velocity_lattice, p.density_lattice, op)
            }
            _ => reconstruct(
                source,
                p.axis,
                p.sign,
                p.velocity_lattice,
                acceleration_lattice,
                p.density_lattice,
            ),
        }
    }


    pub fn cell_jacobian(&self, source: &[f64], acceleration_lattice: [f64; 3]) -> CaeResult<Vec<f64>> {
        if let Some(j) = &self.linear {
            return Ok(j.clone());
        }
        let jac = implexity_ad::forward::jacobian::<Q, _>(
            |x: &[Dual<Q>]| {
                let cell: Cell<Dual<Q>> = std::array::from_fn(|q| x[q]);
                self.apply_cell(&cell, acceleration_lattice).to_vec()
            },
            source,
        )
        .map_err(|e| err(&e.to_string()))?;
        Ok(jac.matrix)
    }
}

pub fn apply_ports_in_place(
    ports: &[CompiledPort],
    state: &mut [f64],
    acceleration_lattice: [f64; 3],
) -> Vec<(usize, f64)> {
    let mut exchange = Vec::new();
    for port in ports {
        for (&cell, &source) in port.port.cells.iter().zip(&port.sources) {
            let s: Cell<f64> = std::array::from_fn(|q| state[source * Q + q]);
            let values = port.apply_cell(&s, acceleration_lattice);
            let mut delta = 0.0;
            for q in 0..Q {
                delta += values[q] - state[cell * Q + q];
                state[cell * Q + q] = values[q];
            }
            exchange.push((cell, delta));
        }
    }
    exchange
}


pub fn apply_ports_vjp(
    ports: &[CompiledPort],
    streamed: &[f64],
    out_bar: &[f64],
    exchange_bar: Option<&[f64]>,
    acceleration_lattice: [f64; 3],
) -> CaeResult<Vec<f64>> {
    let mut bar = out_bar.to_vec();
    for port in ports {
        for &cell in &port.port.cells {
            bar[cell * Q..(cell + 1) * Q].fill(0.0);
        }
    }
    let mut j = 0usize;
    for port in ports {
        for (&cell, &source) in port.port.cells.iter().zip(&port.sources) {
            let e = exchange_bar.map_or(0.0, |e| e[j]);
            j += 1;
            let s = &streamed[source * Q..(source + 1) * Q];
            let g: [f64; Q] = std::array::from_fn(|r| out_bar[cell * Q + r] + e);
            if g.iter().any(|v| *v != 0.0) {
                let jac = port.cell_jacobian(s, acceleration_lattice)?;
                for r in 0..Q {
                    if g[r] == 0.0 {
                        continue;
                    }
                    for k in 0..Q {
                        bar[source * Q + k] += jac[r * Q + k] * g[r];
                    }
                }
            }
            if e != 0.0 {
                for q in 0..Q {
                    bar[cell * Q + q] -= e;
                }
            }
        }
    }
    Ok(bar)
}

