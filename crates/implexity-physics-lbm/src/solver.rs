// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use ndarray::ArrayD;
use rayon::prelude::*;
use serde_json::{Map, Value, json};

use crate::d3q19::{C, Grid, OPPOSITE, Q, W, c};
use crate::design::{DesignMap, TopologyMap, resistance, resistance_derivative};
use crate::geometry;
use crate::nparray::{asarray, py_int};
use crate::ports::{
    Cell, CompiledPort, PortContext, apply_ports_in_place, apply_ports_vjp, dot_c, moments, normalize_ports,
};

pub const METHOD_VERSION: &str = "d3q19-bgk-guo-bounceback-porous-v1";

pub const PROBLEM_KEYS: [&str; 15] = [
    "acceleration_m_s2",
    "density_kg_m3",
    "design_region",
    "drag_max_per_s",
    "drag_shape",
    "fixed_design",
    "history_byte_budget",
    "kinematic_viscosity_m2_s",
    "mach_limit",
    "periodic_axes",
    "shape",
    "solid_mask",
    "spacing_m",
    "step_count",
    "step_s",
];

pub const OPTIONAL_KEYS: [&str; 5] =
    ["topology_map", "ports", "origin_m", "field_registration", "geometry_source"];

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Debug)]
pub struct Streaming {
    grid: Grid,
    solid: Vec<bool>,
    blocked: Vec<bool>,
    source: Vec<u32>,
    readers: Vec<[u32; 2]>,
}

const NONE: u32 = u32::MAX;

impl Streaming {
    #[must_use]
    pub fn new(grid: Grid, periodic: [bool; 3], solid: &[bool]) -> Self {
        let n = grid.cells();
        let mut blocked = vec![false; n * Q];
        for x in 0..n {
            for q in 0..Q {
                let back = [-i64::from(C[q][0]), -i64::from(C[q][1]), -i64::from(C[q][2])];
                blocked[x * Q + q] = solid[grid.wrap(x, back)] || !grid.inside(x, back, periodic);
            }
        }
        let mut source = vec![NONE; n * Q];
        let mut readers = vec![[NONE; 2]; n * Q];
        for x in 0..n {
            if solid[x] {
                continue;
            }
            for q in 0..Q {
                let from = if blocked[x * Q + q] {
                    x * Q + OPPOSITE[q]
                } else {
                    let back = [-i64::from(C[q][0]), -i64::from(C[q][1]), -i64::from(C[q][2])];
                    grid.wrap(x, back) * Q + q
                };
                #[allow(clippy::cast_possible_truncation)]
                {
                    source[x * Q + q] = from as u32;
                    let slot = &mut readers[from];
                    if slot[0] == NONE {
                        slot[0] = (x * Q + q) as u32;
                    } else {
                        slot[1] = (x * Q + q) as u32;
                    }
                }
            }
        }
        Self { grid, solid: solid.to_vec(), blocked, source, readers }
    }

    #[must_use]
    pub fn blocked(&self) -> &[bool] {
        &self.blocked
    }

    #[must_use]
    pub fn solid(&self) -> &[bool] {
        &self.solid
    }

    pub fn stream(&self, post: &[f64], previous: &[f64], out: &mut [f64]) {
        out.par_iter_mut().enumerate().for_each(|(i, o)| {
            let s = self.source[i];
            *o = if s == NONE { previous[i] } else { post[s as usize] };
        });
    }

    pub fn stream_vjp(&self, out_bar: &[f64], post_bar: &mut [f64], previous_bar: &mut [f64]) {
        post_bar.par_iter_mut().enumerate().for_each(|(i, p)| {
            let r = self.readers[i];
            let mut acc = 0.0;
            if r[0] != NONE {
                acc += out_bar[r[0] as usize];
            }
            if r[1] != NONE {
                acc += out_bar[r[1] as usize];
            }
            *p = acc;
        });
        previous_bar.par_chunks_mut(Q).enumerate().for_each(|(x, chunk)| {
            if self.solid[x] {
                chunk.copy_from_slice(&out_bar[x * Q..(x + 1) * Q]);
            } else {
                chunk.fill(0.0);
            }
        });
    }

    #[must_use]
    pub fn grid(&self) -> Grid {
        self.grid
    }

    #[inline]
    #[must_use]
    pub fn source_of(&self, index: usize) -> Option<usize> {
        let s = self.source[index];
        (s != NONE).then_some(s as usize)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Tau<'a> {
    Uniform(f64),
    PerCell(&'a [f64]),
}

impl Tau<'_> {
    #[inline]
    #[must_use]
    pub fn at(&self, cell: usize) -> f64 {
        match self {
            Self::Uniform(t) => *t,
            Self::PerCell(t) => t[cell],
        }
    }
}

#[inline]
pub fn macroscopic<S: Scalar>(f: &Cell<S>, alpha: S, a: [f64; 3]) -> (S, [S; 3], [S; 3]) {
    let (rho, m) = moments(f);
    let s = alpha * 0.5 + 1.0;
    let u: [S; 3] = std::array::from_fn(|d| (m[d] / rho + 0.5 * a[d]) / s);
    let force: [S; 3] = std::array::from_fn(|d| rho * (S::from_f64(a[d]) - alpha * u[d]));
    (rho, u, force)
}

#[inline]
pub fn collide_cell<S: Scalar>(f: &Cell<S>, alpha: S, a: [f64; 3], tau: S) -> Cell<S> {
    let (rho, u, force) = macroscopic(f, alpha, a);
    let uf = u[0] * force[0] + u[1] * force[1] + u[2] * force[2];
    let eq = crate::ports::equilibrium(rho, u);
    let k = S::one() - S::from_f64(0.5) / tau;
    std::array::from_fn(|q| {
        let cu = dot_c(q, u);
        let cf = dot_c(q, force);
        let source = k * W[q] * (cf * 3.0 - uf * 3.0 + cu * cf * 9.0);
        f[q] - (f[q] - eq[q]) / tau + source
    })
}

#[inline]
#[must_use]
pub fn collide_cell_vjp(
    f: &Cell<f64>,
    alpha: f64,
    a: [f64; 3],
    tau: f64,
    g: &Cell<f64>,
) -> (Cell<f64>, f64, f64) {
    let (rho, m) = moments(f);
    let s = 1.0 + 0.5 * alpha;
    let u: [f64; 3] = std::array::from_fn(|d| (m[d] / rho + 0.5 * a[d]) / s);
    let force: [f64; 3] = std::array::from_fn(|d| rho * (a[d] - alpha * u[d]));
    let uf = u[0] * force[0] + u[1] * force[1] + u[2] * force[2];
    let usq = u[0] * u[0] + u[1] * u[1] + u[2] * u[2];
    let k = 1.0 - 0.5 / tau;
    let mut f_bar = [0.0; Q];
    let (mut rho_bar, mut tau_bar, mut uf_bar, mut usq_bar) = (0.0, 0.0, 0.0, 0.0);
    let mut u_bar = [0.0; 3];
    let mut force_bar = [0.0; 3];
    for q in 0..Q {
        let gq = g[q];
        if gq == 0.0 {
            continue;
        }
        let cu = dot_c(q, u);
        let cf = dot_c(q, force);
        let poly = 1.0 + 3.0 * cu + 4.5 * cu * cu - 1.5 * usq;
        let eq = rho * W[q] * poly;
        f_bar[q] += gq * (1.0 - 1.0 / tau);
        let eq_bar = gq / tau;
        tau_bar += gq * (f[q] - eq) / (tau * tau)
            + gq * W[q] * (0.5 / (tau * tau)) * (3.0 * cf - 3.0 * uf + 9.0 * cu * cf);
        let h = gq * W[q] * k;
        let cf_bar = h * (3.0 + 9.0 * cu);
        let mut cu_bar = h * 9.0 * cf;
        uf_bar -= 3.0 * h;
        rho_bar += eq_bar * W[q] * poly;
        let e = eq_bar * rho * W[q];
        cu_bar += e * (3.0 + 9.0 * cu);
        usq_bar -= 1.5 * e;
        for d in 0..3 {
            let cd = c(q, d);
            u_bar[d] += cu_bar * cd;
            force_bar[d] += cf_bar * cd;
        }
    }
    for d in 0..3 {
        u_bar[d] += usq_bar * 2.0 * u[d] + uf_bar * force[d];
        force_bar[d] += uf_bar * u[d];
    }
    let mut alpha_bar = 0.0;
    for d in 0..3 {
        rho_bar += force_bar[d] * (a[d] - alpha * u[d]);
        alpha_bar -= force_bar[d] * rho * u[d];
        u_bar[d] -= force_bar[d] * rho * alpha;
    }
    let mut m_bar = [0.0; 3];
    let mut s_bar = 0.0;
    for d in 0..3 {
        m_bar[d] = u_bar[d] / (s * rho);
        rho_bar -= u_bar[d] * m[d] / (s * rho * rho);
        s_bar -= u_bar[d] * u[d] / s;
    }
    alpha_bar += 0.5 * s_bar;
    for q in 0..Q {
        let mut v = f_bar[q] + rho_bar;
        for d in 0..3 {
            v += m_bar[d] * c(q, d);
        }
        f_bar[q] = v;
    }
    (f_bar, alpha_bar, tau_bar)
}

const CF: [[f64; 3]; Q] = {
    let mut out = [[0.0; 3]; Q];
    let mut q = 0;
    while q < Q {
        out[q] = [C[q][0] as f64, C[q][1] as f64, C[q][2] as f64];
        q += 1;
    }
    out
};

#[inline]
pub fn collide_cell_f64(f: &[f64], alpha: f64, a: [f64; 3], tau: f64, out: &mut [f64]) {
    let mut rho = f[0];
    for v in &f[1..Q] {
        rho += *v;
    }
    let (mut mx, mut my, mut mz) = (0.0, 0.0, 0.0);
    for q in 0..Q {
        mx += f[q] * CF[q][0];
        my += f[q] * CF[q][1];
        mz += f[q] * CF[q][2];
    }
    let s = alpha * 0.5 + 1.0;
    let u = [(mx / rho + 0.5 * a[0]) / s, (my / rho + 0.5 * a[1]) / s, (mz / rho + 0.5 * a[2]) / s];
    let force = [rho * (a[0] - alpha * u[0]), rho * (a[1] - alpha * u[1]), rho * (a[2] - alpha * u[2])];
    let uf = u[0] * force[0] + u[1] * force[1] + u[2] * force[2];
    let usq = u[0] * u[0] + u[1] * u[1] + u[2] * u[2];
    let k = 1.0 - 0.5 / tau;
    for q in 0..Q {
        let cu = 0.0 + u[0] * CF[q][0] + u[1] * CF[q][1] + u[2] * CF[q][2];
        let cf = 0.0 + force[0] * CF[q][0] + force[1] * CF[q][1] + force[2] * CF[q][2];
        let eq = rho * W[q] * (1.0 + cu * 3.0 + cu * cu * 4.5 - usq * 1.5);
        let source = k * W[q] * (cf * 3.0 - uf * 3.0 + cu * cf * 9.0);
        out[q] = f[q] - (f[q] - eq) / tau + source;
    }
}

pub fn collide(f: &[f64], alpha: &[f64], a: [f64; 3], tau: Tau<'_>, out: &mut [f64]) {
    out.par_chunks_mut(Q).zip(f.par_chunks(Q)).enumerate().for_each(|(x, (o, cell))| {
        collide_cell_f64(cell, alpha[x], a, tau.at(x), o);
    });
}

#[must_use]
pub fn collide_vjp(
    f: &[f64],
    alpha: &[f64],
    a: [f64; 3],
    tau: Tau<'_>,
    post_bar: &[f64],
) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let n = alpha.len();
    let mut f_bar = vec![0.0; n * Q];
    let mut alpha_bar = vec![0.0; n];
    let mut tau_bar = vec![0.0; n];
    f_bar.par_chunks_mut(Q).zip(alpha_bar.par_iter_mut()).zip(tau_bar.par_iter_mut()).enumerate().for_each(
        |(x, ((fb, ab), tb))| {
            let g: Cell<f64> = std::array::from_fn(|q| post_bar[x * Q + q]);
            if g.iter().all(|v| *v == 0.0) {
                return;
            }
            let cell: Cell<f64> = std::array::from_fn(|q| f[x * Q + q]);
            let (df, da, dt) = collide_cell_vjp(&cell, alpha[x], a, tau.at(x), &g);
            fb.copy_from_slice(&df);
            *ab = da;
            *tb = dt;
        },
    );
    (f_bar, alpha_bar, tau_bar)
}

#[derive(Clone, Debug)]
pub struct LbmProblem {
    pub grid: Grid,
    pub spacing_m: f64,
    pub step_s: f64,
    pub step_count: usize,
    pub density_kg_m3: f64,
    pub kinematic_viscosity_m2_s: f64,
    pub acceleration_m_s2: [f64; 3],
    pub periodic_axes: [bool; 3],
    pub solid_mask: Vec<bool>,
    pub design_region: Vec<bool>,
    pub fixed_design: Vec<f64>,
    pub drag_max_per_s: f64,
    pub drag_shape: f64,
    pub mach_limit: f64,
    pub history_byte_budget: i64,
    pub tau: f64,
    pub topology_map: TopologyMap,
    pub ports: Vec<CompiledPort>,
    pub origin_m: [f64; 3],
    pub geometry_sampling: Option<Value>,
    pub design_map: DesignMap,
    pub streaming: Streaming,
}

fn positive_scalar(problem: &Map<String, Value>, key: &str) -> CaeResult<f64> {
    match asarray(problem.get(key).unwrap_or(&Value::Null)).real_scalar() {
        Some(v) if v > 0.0 => Ok(v),
        _ => Err(err(format!("{key} must be a finite positive scalar"))),
    }
}

fn shape_of(value: &Value) -> Option<[usize; 3]> {
    let list = value.as_array().filter(|l| l.len() == 3)?;
    let mut shape = [0usize; 3];
    for (s, v) in shape.iter_mut().zip(list) {
        let n = py_int(v).filter(|n| *n >= 2)?;
        *s = usize::try_from(n).ok()?;
    }
    Some(shape)
}

#[must_use]
pub fn periodic_of(value: &Value) -> Option<[bool; 3]> {
    let list = value.as_array().filter(|l| l.len() == 3)?;
    let mut out = [false; 3];
    for (o, v) in out.iter_mut().zip(list) {
        *o = v.as_bool()?;
    }
    Some(out)
}

impl LbmProblem {

    #[allow(clippy::too_many_lines)]
    pub fn normalize(problem: &Value) -> CaeResult<Self> {
        let keys_ok = problem.as_object().is_some_and(|m| {
            let mut keys: Vec<&str> =
                m.keys().map(String::as_str).filter(|k| !OPTIONAL_KEYS.contains(k)).collect();
            keys.sort_unstable();
            keys == PROBLEM_KEYS
        });
        let Some(p) = problem.as_object().filter(|_| keys_ok) else {
            let list: Vec<String> = PROBLEM_KEYS.iter().map(|k| format!("'{k}'")).collect();
            return Err(err(format!("LBM requires exactly these problem keys: [{}]", list.join(", "))));
        };
        let Some(shape) = shape_of(&p["shape"]) else {
            return Err(err("LBM shape must contain three integers >= 2"));
        };
        let spacing_m = positive_scalar(p, "spacing_m")?;
        let step_s = positive_scalar(p, "step_s")?;
        let density_kg_m3 = positive_scalar(p, "density_kg_m3")?;
        let kinematic_viscosity_m2_s = positive_scalar(p, "kinematic_viscosity_m2_s")?;
        let drag_max_per_s = positive_scalar(p, "drag_max_per_s")?;
        let drag_shape = positive_scalar(p, "drag_shape")?;
        let mach_limit = positive_scalar(p, "mach_limit")?;
        if mach_limit > 0.2 {
            return Err(err("LBM mach_limit must be <= 0.2; this is not a compressible gas solver"));
        }
        let count = py_int(&p["step_count"]).filter(|c| (1..=1_000_000).contains(c));
        let Some(count) = count else {
            return Err(err("step_count must be an integer in [1,1000000]"));
        };
        let cells = shape[0] * shape[1] * shape[2];
        let required = i128::from(count + 1) * cells as i128 * 19 * 8;
        let budget = py_int(&p["history_byte_budget"]).filter(|b| i128::from(*b) >= required);
        let Some(budget) = budget else {
            return Err(err(
                "Population history exceeds explicit byte budget; AD working memory is additional",
            ));
        };
        let grid = Grid::new(shape);
        let mut geometry_sampling = None;
        let (solid_mask, design_region, fixed_raw) = if let Some(source) = p.get("geometry_source") {
            let nulls = ["solid_mask", "design_region", "fixed_design"].iter().all(|k| p[*k].is_null());
            let origin_m = geometry::origin(p.get("origin_m").unwrap_or(&json!([0.0, 0.0, 0.0])))?;
            let masks = geometry::masks_from_document(shape, spacing_m, origin_m, source, nulls)?;
            geometry_sampling = Some(masks.sampling);
            (masks.solid, masks.design, masks.fixed)
        } else {
            let mut masks = Vec::new();
            for key in ["solid_mask", "design_region"] {
                let a = asarray(&p[key]);
                if a.shape != shape || !a.is_bool() {
                    return Err(err(format!("{key} must be a shape-matched boolean array")));
                }
                masks.push(a.bools());
            }
            let fixed = asarray(&p["fixed_design"]);
            if fixed.shape != shape
                || !fixed.is_real()
                || !fixed.all_finite()
                || fixed.data.iter().any(|v| !(0.0..=1.0).contains(v))
            {
                return Err(err("fixed_design must be finite and in [0,1]"));
            }
            let design = masks.pop().unwrap_or_default();
            let solid = masks.pop().unwrap_or_default();
            (solid, design, fixed.data)
        };
        if solid_mask.iter().all(|s| *s) || solid_mask.iter().zip(&design_region).any(|(s, d)| *s && *d) {
            return Err(err("Need fluid cells; fixed solid cells cannot be designable"));
        }
        let acc = asarray(&p["acceleration_m_s2"]);
        if acc.shape != [3] || !acc.is_real() || !acc.all_finite() {
            return Err(err("acceleration_m_s2 requires three finite components"));
        }
        let acceleration_m_s2 = [acc.data[0], acc.data[1], acc.data[2]];
        let Some(periodic_axes) = periodic_of(&p["periodic_axes"]) else {
            return Err(err("periodic_axes requires three booleans; false means closed no-slip walls"));
        };
        let tau = 0.5 + 3.0 * kinematic_viscosity_m2_s * step_s / (spacing_m * spacing_m);
        if !(0.51..=2.0).contains(&tau) {
            return Err(err("Choose spacing/time step such that 0.51 <= relaxation time <= 2"));
        }
        let default_map = json!({"filter_radius_m": 0.0, "projection_beta": 0.0, "projection_eta": 0.5});
        let topology_map = TopologyMap::normalize(p.get("topology_map").unwrap_or(&default_map), spacing_m)?;
        let ctx = PortContext {
            grid,
            periodic: periodic_axes,
            solid: &solid_mask,
            design_region: &design_region,
            fixed_design: &fixed_raw,
            acceleration_m_s2,
            step_s,
            spacing_m,
            mach_limit,
            density_kg_m3,
        };
        let raw_ports = normalize_ports(p.get("ports").unwrap_or(&json!([])), &ctx)?;
        let origin_m = geometry::origin(p.get("origin_m").unwrap_or(&json!([0.0, 0.0, 0.0])))?;
        geometry::check_registration(p, shape, spacing_m, origin_m)?;
        let a_lattice: [f64; 3] = acceleration_m_s2.map(|v| v * step_s * step_s / spacing_m);
        let ports = raw_ports
            .into_iter()
            .map(|port| CompiledPort::new(port, &grid, a_lattice))
            .collect::<CaeResult<Vec<_>>>()?;
        let design_map = DesignMap::new(
            grid,
            periodic_axes,
            &solid_mask,
            &design_region,
            &fixed_raw,
            spacing_m,
            &topology_map,
        );
        let streaming = Streaming::new(grid, periodic_axes, &solid_mask);
        Ok(Self {
            grid,
            spacing_m,
            step_s,
            step_count: usize::try_from(count).unwrap_or(1),
            density_kg_m3,
            kinematic_viscosity_m2_s,
            acceleration_m_s2,
            periodic_axes,
            solid_mask,
            design_region,
            fixed_design: fixed_raw,
            drag_max_per_s,
            drag_shape,
            mach_limit,
            history_byte_budget: budget,
            tau,
            topology_map,
            ports,
            origin_m,
            geometry_sampling,
            design_map,
            streaming,
        })
    }

    #[must_use]
    pub fn shape(&self) -> [usize; 3] {
        self.grid.shape
    }

    #[must_use]
    pub fn cells(&self) -> usize {
        self.grid.cells()
    }

    #[must_use]
    pub fn acceleration_lattice(&self) -> [f64; 3] {
        self.acceleration_m_s2.map(|v| v * self.step_s * self.step_s / self.spacing_m)
    }


    pub fn checked_design(&self, raw: &ArrayD<f64>) -> CaeResult<Vec<f64>> {
        if raw.shape() != self.grid.shape || raw.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
            return Err(err("model:control must be a finite shape-matched array in [0,1]"));
        }
        Ok(raw.iter().copied().collect())
    }

    #[must_use]
    pub fn fraction(&self, raw: &[f64]) -> Vec<f64> {
        self.design_map.map(raw)
    }

    #[must_use]
    pub fn resistance(&self, phi: &[f64]) -> Vec<f64> {
        phi.iter().map(|&v| resistance(self.step_s, self.drag_max_per_s, self.drag_shape, v)).collect()
    }

    #[must_use]
    pub fn resistance_derivative(&self, phi: &[f64]) -> Vec<f64> {
        phi.iter()
            .map(|&v| resistance_derivative(self.step_s, self.drag_max_per_s, self.drag_shape, v))
            .collect()
    }

    #[must_use]
    pub fn initial_populations(&self) -> Vec<f64> {
        let a = self.acceleration_lattice();
        let fluid: Cell<f64> = std::array::from_fn(|q| W[q] - 1.5 * W[q] * dot_c(q, a));
        let mut f0 = vec![0.0; self.cells() * Q];
        for (x, chunk) in f0.chunks_mut(Q).enumerate() {
            chunk.copy_from_slice(if self.solid_mask[x] { &W } else { &fluid });
        }
        f0
    }

    #[must_use]
    pub fn interval(&self, before: &[f64], alpha: &[f64], tau: Tau<'_>) -> Interval {
        let a = self.acceleration_lattice();
        let n = before.len();
        let mut post = vec![0.0; n];
        collide(before, alpha, a, tau, &mut post);
        let mut after = vec![0.0; n];
        self.streaming.stream(&post, before, &mut after);
        let port_exchange = apply_ports_in_place(&self.ports, &mut after, a);
        let exchanged_density = port_exchange.iter().map(|e| e.1).sum();
        Interval { post, after, exchanged_density, port_exchange }
    }

    #[must_use]
    pub fn reflected_cells(&self, before: &[f64], alpha: &[f64], tau: Tau<'_>, cells: &[usize]) -> Vec<f64> {
        let a = self.acceleration_lattice();
        let mut out = vec![0.0; before.len()];
        let mut post_cache: std::collections::BTreeMap<usize, [f64; Q]> = std::collections::BTreeMap::new();
        for &y in cells {
            for q in 0..Q {
                let i = y * Q + q;
                out[i] = match self.streaming.source_of(i) {
                    None => before[i],
                    Some(from) => {
                        let z = from / Q;
                        let post = post_cache.entry(z).or_insert_with(|| {
                            let mut o = [0.0; Q];
                            collide_cell_f64(&before[z * Q..(z + 1) * Q], alpha[z], a, tau.at(z), &mut o);
                            o
                        });
                        post[from % Q]
                    }
                };
            }
        }
        out
    }


    pub fn interval_vjp(
        &self,
        before: &[f64],
        alpha: &[f64],
        tau: Tau<'_>,
        after_bar: &[f64],
        post_bar_extra: Option<&[f64]>,
        exchange_bar: Option<&[f64]>,
    ) -> CaeResult<(Vec<f64>, Vec<f64>, Vec<f64>)> {
        let a = self.acceleration_lattice();
        let n = before.len();
        let reflected_bar = if self.ports.is_empty() {
            after_bar.to_vec()
        } else {
            let mut cells: Vec<usize> = self.ports.iter().flat_map(|p| p.sources.iter().copied()).collect();
            cells.extend(self.ports.iter().flat_map(|p| p.port.cells.iter().copied()));
            let reflected = self.reflected_cells(before, alpha, tau, &cells);
            apply_ports_vjp(&self.ports, &reflected, after_bar, exchange_bar, a)?
        };
        let mut post_bar = vec![0.0; n];
        let mut previous_bar = vec![0.0; n];
        self.streaming.stream_vjp(&reflected_bar, &mut post_bar, &mut previous_bar);
        if let Some(extra) = post_bar_extra {
            for (p, e) in post_bar.iter_mut().zip(extra) {
                *p += e;
            }
        }
        let (mut f_bar, alpha_bar, tau_bar) = collide_vjp(before, alpha, a, tau, &post_bar);
        for (v, p) in f_bar.iter_mut().zip(&previous_bar) {
            *v += p;
        }
        Ok((f_bar, alpha_bar, tau_bar))
    }

    #[must_use]
    pub fn history(&self, alpha: &[f64], tau: Tau<'_>) -> History {
        let mut states = Vec::with_capacity(self.step_count + 1);
        let mut boundary = Vec::with_capacity(self.step_count);
        states.push(self.initial_populations());
        for t in 0..self.step_count {
            let interval = self.interval(&states[t], alpha, tau);
            boundary.push(interval.exchanged_density);
            states.push(interval.after);
        }
        History { states, boundary_mass_changes_lattice: boundary }
    }

    #[must_use]
    pub fn fields(&self, alpha: &[f64], populations: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let a = self.acceleration_lattice();
        let scale = self.spacing_m / self.step_s;
        let n = self.cells();
        let mut rho = vec![0.0; n];
        let mut u = vec![0.0; 3 * n];
        for x in 0..n {
            let cell: Cell<f64> = std::array::from_fn(|q| populations[x * Q + q]);
            let (r, v, _) = macroscopic(&cell, alpha[x], a);
            rho[x] = r * self.density_kg_m3;
            for d in 0..3 {
                u[3 * x + d] = if self.solid_mask[x] { 0.0 } else { v[d] } * scale;
            }
        }
        (rho, u)
    }


    pub fn admit(
        &self,
        alpha: &[f64],
        history: &History,
        explicit_boundary: bool,
    ) -> CaeResult<Map<String, Value>> {
        let states = &history.states;
        let fluid: Vec<usize> = (0..self.cells()).filter(|x| !self.solid_mask[*x]).collect();
        let mut minimum = f64::INFINITY;
        for state in states {
            if !state.iter().all(|v| f64::is_finite(*v)) {
                return Err(err("LBM trajectory has nonpositive or nonfinite populations"));
            }
            for &x in &fluid {
                for q in 0..Q {
                    minimum = minimum.min(state[x * Q + q]);
                }
            }
        }
        if minimum <= 0.0 {
            return Err(err("LBM trajectory has nonpositive or nonfinite populations"));
        }
        let a = self.acceleration_lattice();
        let per_state: Vec<(f64, f64)> = states
            .par_iter()
            .map(|state| {
                let mut total = 0.0;
                let mut worst: f64 = 0.0;
                for &x in &fluid {
                    let cell: Cell<f64> = std::array::from_fn(|q| state[x * Q + q]);
                    let (rho, u, _) = macroscopic(&cell, alpha[x], a);
                    worst = worst.max((u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt());
                    total += rho;
                }
                (worst, total)
            })
            .collect();
        let mut mach = per_state.iter().fold(0.0_f64, |m, s| m.max(s.0));
        let mass: Vec<f64> = per_state.iter().map(|s| s.1).collect();
        mach *= 3f64.sqrt();
        let changes: Vec<f64> = if explicit_boundary || !self.ports.is_empty() {
            history.boundary_mass_changes_lattice.clone()
        } else {
            Vec::new()
        };
        if changes.iter().any(|v| !v.is_finite()) {
            return Err(err("Invalid boundary mass exchange history"));
        }
        let mut error: f64 = 0.0;
        let mut cumulative = 0.0;
        for (t, m) in mass.iter().enumerate() {
            if t > 0 && !changes.is_empty() {
                cumulative += changes[t - 1];
            }
            error = error.max((m - (mass[0] + cumulative)).abs());
        }
        let error = error / mass[0];
        if mach > self.mach_limit || error > 1e-6 {
            return Err(err(format!(
                "LBM trajectory rejected: Mach={}, mass drift={}",
                implexity_core::py_repr::repr_float(mach),
                implexity_core::py_repr::repr_float(error)
            )));
        }
        let unit = self.density_kg_m3 * self.spacing_m.powi(3);
        let mut d = Map::new();
        d.insert("maximum_mach".into(), json!(mach));
        d.insert("relative_mass_drift".into(), json!(error));
        d.insert("relaxation_time".into(), json!(self.tau));
        d.insert("physical_qualification".into(), json!(false));
        d.insert("steady_state_certified".into(), json!(false));
        d.insert(
            "derivative_scope".into(),
            json!("Discrete fixed-horizon porous-resistance trajectory; fixed walls and forcing"),
        );
        d.insert("method".into(), json!("Independent D3Q19 BGK / Guo / halfway bounce-back"));
        d.insert("method_version".into(), json!(METHOD_VERSION));
        d.insert("population_dtype".into(), json!("float64"));
        d.insert(
            "boundary_population_mass_exchange_kg".into(),
            json!(changes.iter().map(|v| v * unit).collect::<Vec<_>>()),
        );
        d.insert("step_count".into(), json!(self.step_count));
        d.insert("end_time_s".into(), json!(self.step_count as f64 * self.step_s));
        Ok(d)
    }
}

impl LbmProblem {

    pub fn history_adjoint(
        &self,
        history: &History,
        alpha: &[f64],
        tau: Tau<'_>,
        lambda_final: Vec<f64>,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        let mut lambda = lambda_final;
        let mut alpha_bar = vec![0.0; alpha.len()];
        for t in (0..self.step_count).rev() {
            let (f_bar, a_bar, _) = self.interval_vjp(&history.states[t], alpha, tau, &lambda, None, None)?;
            for (acc, v) in alpha_bar.iter_mut().zip(&a_bar) {
                *acc += v;
            }
            lambda = f_bar;
        }
        Ok((alpha_bar, lambda))
    }
}

#[derive(Clone, Debug)]
pub struct Interval {
    pub post: Vec<f64>,
    pub after: Vec<f64>,
    pub exchanged_density: f64,
    pub port_exchange: Vec<(usize, f64)>,
}

#[derive(Clone, Debug)]
pub struct History {
    pub states: Vec<Vec<f64>>,
    pub boundary_mass_changes_lattice: Vec<f64>,
}

pub const VECTOR_LEN: usize = 6;

#[inline]
pub fn vector_cell<S: Scalar>(
    f: &Cell<S>,
    alpha: S,
    phi: S,
    solid: bool,
    a: [f64; 3],
    p: &VectorScales,
) -> [S; 6] {
    let (rho, u, _) = macroscopic(f, alpha, a);
    let u: [S; 3] = std::array::from_fn(|d| if solid { S::zero() } else { u[d] } * p.velocity);
    let rho = rho * p.density;
    let usq = u[0] * u[0] + u[1] * u[1] + u[2] * u[2];
    let (kinetic, drag) =
        if solid { (S::zero(), S::zero()) } else { (rho * usq, rho * alpha / p.step_s * usq) };
    [u[0], u[1], u[2], kinetic, drag, phi]
}

#[derive(Clone, Copy, Debug)]
pub struct VectorScales {
    pub velocity: f64,
    pub density: f64,
    pub step_s: f64,
    pub dv: f64,
    pub fluid_cells: f64,
}

impl LbmProblem {
    #[must_use]
    pub fn vector_scales(&self) -> VectorScales {
        VectorScales {
            velocity: self.spacing_m / self.step_s,
            density: self.density_kg_m3,
            step_s: self.step_s,
            dv: self.spacing_m.powi(3),
            fluid_cells: self.solid_mask.iter().filter(|s| !**s).count() as f64,
        }
    }

    #[must_use]
    pub fn vector(&self, phi: &[f64], alpha: &[f64], final_state: &[f64]) -> [f64; VECTOR_LEN] {
        let a = self.acceleration_lattice();
        let s = self.vector_scales();
        let mut sums = [0.0; 6];
        for x in 0..self.cells() {
            let cell: Cell<f64> = std::array::from_fn(|q| final_state[x * Q + q]);
            let c = vector_cell(&cell, alpha[x], phi[x], self.solid_mask[x], a, &s);
            for k in 0..6 {
                sums[k] += c[k];
            }
        }
        [
            sums[0] / s.fluid_cells,
            sums[1] / s.fluid_cells,
            sums[2] / s.fluid_cells,
            0.5 * sums[3] * s.dv,
            sums[4] * s.dv,
            sums[5] * s.dv,
        ]
    }

    #[must_use]
    pub fn vector_vjp(
        &self,
        phi: &[f64],
        alpha: &[f64],
        final_state: &[f64],
        w: &[f64; VECTOR_LEN],
    ) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let a = self.acceleration_lattice();
        let s = self.vector_scales();
        let weights = [
            w[0] / s.fluid_cells,
            w[1] / s.fluid_cells,
            w[2] / s.fluid_cells,
            0.5 * w[3] * s.dv,
            w[4] * s.dv,
            w[5] * s.dv,
        ];
        let n = self.cells();
        let mut f_bar = vec![0.0; n * Q];
        let mut alpha_bar = vec![0.0; n];
        let mut phi_bar = vec![0.0; n];
        f_bar
            .par_chunks_mut(Q)
            .zip(alpha_bar.par_iter_mut())
            .zip(phi_bar.par_iter_mut())
            .enumerate()
            .for_each(|(x, ((fb, ab), pb))| {
                let mut input = [0.0; Q + 2];
                input[..Q].copy_from_slice(&final_state[x * Q..(x + 1) * Q]);
                input[Q] = alpha[x];
                input[Q + 1] = phi[x];
                let solid = self.solid_mask[x];
                let gradient = implexity_ad::forward::gradient::<{ Q + 2 }, _>(
                    |v| {
                        let cell: Cell<_> = std::array::from_fn(|q| v[q]);
                        let c = vector_cell(&cell, v[Q], v[Q + 1], solid, a, &s);
                        let mut acc = c[0] * weights[0];
                        for k in 1..6 {
                            acc += c[k] * weights[k];
                        }
                        acc
                    },
                    &input,
                );
                if let Ok((_, g)) = gradient {
                    fb.copy_from_slice(&g[..Q]);
                    *ab = g[Q];
                    *pb = g[Q + 1];
                }
            });
        (f_bar, alpha_bar, phi_bar)
    }
}

