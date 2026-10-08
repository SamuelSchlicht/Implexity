// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::Arc;

use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_physics_solid::solid_history::SolidKernel;
use implexity_solve::coupled_history::{HistoryBlockCallbacks, HistoryInterface};
use implexity_solve::local_assembly::{Kind, LocalResidual};
use implexity_solve::matrix::Jacobian;

use crate::incompressible_transport::{FluidKernel, flat, kernel_ndindex};
use crate::local_group::{GroupOps, StepKernel, group, sum_jacobians};

pub const KEY: &str = "wall_film";
pub const BLOCK: &str = "wall_film";
pub const MODEL: &str = "gnielinski_two_temperature";
pub use implexity_physics_base::wall_film_temperature::{LAMINAR_NU, RE_LAMINAR, RE_TURBULENT, nusselt};
pub const SPEED_SMOOTHING_M_S: f64 = 1.0e-2;
pub const GRADIENT_REGULARISATION: f64 = 1.0e-3;
pub const WEIGHT_FLOOR: f64 = 1.0e-6;
pub const WALL_FLUX_AREA_FRACTION: f64 = 0.01;

const KEYS: [&str; 6] = [
    "model",
    "hydraulic_diameter_m",
    "fluid",
    "enhancement_factor",
    "transverse_dispersion_coefficient",
    "provenance",
];

#[derive(Debug, Clone, PartialEq)]
pub struct FilmSettings {
    pub hydraulic_diameter_m: f64,
    pub density: f64,
    pub viscosity: f64,
    pub conductivity: f64,
    pub heat_capacity: f64,
    pub enhancement: f64,
    pub dispersion: f64,
    pub provenance: String,
}

impl FilmSettings {
    #[must_use]
    pub fn prandtl(&self) -> f64 {
        self.heat_capacity * self.viscosity / self.conductivity
    }

    pub fn reynolds<S: Scalar>(&self, speed: S) -> S {
        speed * (self.density * self.hydraulic_diameter_m / self.viscosity)
    }

    pub fn coefficient<S: Scalar>(&self, speed: S) -> S {
        let magnitude = (speed * speed + SPEED_SMOOTHING_M_S * SPEED_SMOOTHING_M_S).sqrt();
        nusselt(self.reynolds(magnitude), self.prandtl())
            * (self.enhancement * self.conductivity / self.hydraulic_diameter_m)
    }

    pub fn dispersion_conductivity<S: Scalar>(&self, speed: S) -> S {
        let magnitude = (speed * speed + SPEED_SMOOTHING_M_S * SPEED_SMOOTHING_M_S).sqrt();
        magnitude * (self.dispersion * self.density * self.heat_capacity * self.hydraulic_diameter_m)
    }
}


pub fn validate(settings: &Value) -> CaeResult<FilmSettings> {
    let err = || {
        CaeError::contract(format!(
            "wall_film requires exactly model '{MODEL}', positive hydraulic_diameter_m, fluid {{density_kg_m3, mu_Pa_s, k_W_mK, cp_J_kgK}} (positive), enhancement_factor >= 1, transverse_dispersion_coefficient >= 0 and provenance"
        ))
    };
    let s = settings.as_object().ok_or_else(err)?;
    if s.len() != KEYS.len() || KEYS.iter().any(|k| !s.contains_key(*k)) {
        return Err(err());
    }
    if s.get("model").and_then(Value::as_str) != Some(MODEL) {
        return Err(err());
    }
    let number =
        |v: Option<&Value>| v.filter(|v| v.is_number()).and_then(Value::as_f64).filter(|v| v.is_finite());
    let positive = |v: Option<&Value>| number(v).filter(|v| *v > 0.0);
    let fluid = s.get("fluid").and_then(Value::as_object).filter(|f| f.len() == 4).ok_or_else(err)?;
    Ok(FilmSettings {
        hydraulic_diameter_m: positive(s.get("hydraulic_diameter_m")).ok_or_else(err)?,
        density: positive(fluid.get("density_kg_m3")).ok_or_else(err)?,
        viscosity: positive(fluid.get("mu_Pa_s")).ok_or_else(err)?,
        conductivity: positive(fluid.get("k_W_mK")).ok_or_else(err)?,
        heat_capacity: positive(fluid.get("cp_J_kgK")).ok_or_else(err)?,
        enhancement: number(s.get("enhancement_factor")).filter(|e| *e >= 1.0).ok_or_else(err)?,
        dispersion: number(s.get("transverse_dispersion_coefficient"))
            .filter(|c| *c >= 0.0)
            .ok_or_else(err)?,
        provenance: s
            .get("provenance")
            .and_then(Value::as_str)
            .filter(|p| !p.trim().is_empty())
            .ok_or_else(err)?
            .to_string(),
    })
}

pub(crate) struct FilmBlock {
    pub size: usize,
    pub design: usize,
}

impl FilmBlock {
    fn zero(&self, cols: usize) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(
            CsrMatrix::from_triplets(self.size, cols, &[], &[], &[])
                .map_err(|e| CaeError::contract(e.to_string()))?,
        ))
    }
}

impl HistoryBlockCallbacks for FilmBlock {
    fn residual(&self, _n: usize, _z: &[f64], _old: &[f64], _x: &[f64]) -> CaeResult<Vec<f64>> {
        Ok(vec![0.0; self.size])
    }
    fn current_jacobian(&self, _n: usize, _z: &[f64], _old: &[f64], _x: &[f64]) -> CaeResult<Jacobian> {
        self.zero(self.size)
    }
    fn previous_jacobian(&self, _n: usize, _z: &[f64], _old: &[f64], _x: &[f64]) -> CaeResult<Jacobian> {
        self.zero(self.size)
    }
    fn design_jacobian(&self, _n: usize, _z: &[f64], _old: &[f64], _x: &[f64]) -> CaeResult<Jacobian> {
        self.zero(self.design)
    }
}

struct Shared {
    settings: FilmSettings,
    t0: f64,
    ts: f64,
    us: f64,
    scale: f64,
    floor: f64,
    grid: [usize; 3],
    fixed_t: Vec<Vec<f64>>,
}

impl Shared {
    fn fraction<S: Scalar>(&self, theta: S) -> S {
        (-theta + 1.0) * (1.0 - self.floor) + self.floor
    }
}

struct ExchangeKernel {
    s: Arc<Shared>,
    nodes: Vec<[usize; 3]>,
}

fn slot(o: [i64; 3]) -> usize {
    let i = |v: i64| usize::try_from(v + 2).unwrap_or(0);
    (i(o[0]) * 4 + i(o[1])) * 4 + i(o[2])
}

impl ExchangeKernel {
    fn nodal_fraction<S: Scalar>(&self, q: [usize; 3], x: &[S], p: [i64; 3]) -> S {
        let mut sum = S::zero();
        let mut count = 0.0;
        for d in [[0i64, 0, 0], [0, 0, 1], [0, 1, 0], [0, 1, 1], [1, 0, 0], [1, 0, 1], [1, 1, 0], [1, 1, 1]] {
            let o = [p[0] - 1 + d[0], p[1] - 1 + d[1], p[2] - 1 + d[2]];
            if self.inside(q, o) {
                sum += x[slot(o)];
                count += 1.0;
            }
        }
        sum * (1.0 / count)
    }

    fn inside(&self, q: [usize; 3], o: [i64; 3]) -> bool {
        (0..3).all(|a| {
            let c = i64::try_from(q[a]).unwrap_or(0) + o[a];
            c >= 0 && c < i64::try_from(self.s.grid[a]).unwrap_or(0)
        })
    }

    fn area<S: Scalar>(&self, item: usize, x: &[S]) -> S {
        let q = self.nodes[item];
        let h: [S; 3] = std::array::from_fn(|a| x[64 + a] * 1e-3);
        let volume = h[0] * h[1] * h[2];
        let hmin = h.iter().fold(f64::INFINITY, |m, v| m.min(v.value()));
        let eps = GRADIENT_REGULARISATION / hmin;
        let theta = |o: [i64; 3]| x[slot(o)];
        let phi = |p: [i64; 3]| self.nodal_fraction(q, x, p);
        let mut total = S::zero();
        for c in [
            [-1i64, -1, -1],
            [-1, -1, 0],
            [-1, 0, -1],
            [-1, 0, 0],
            [0, -1, -1],
            [0, -1, 0],
            [0, 0, -1],
            [0, 0, 0],
        ] {
            if !self.inside(q, c) {
                continue;
            }
            let mut g2 = S::zero();
            for a in 0..3 {
                let mut up = c;
                up[a] += 1;
                let mut down = c;
                down[a] -= 1;
                let hi = if self.inside(q, up) { theta(up) } else { theta(c) };
                let lo = if self.inside(q, down) { theta(down) } else { theta(c) };
                let g = (hi - lo) / (h[a] * 2.0);
                g2 += g * g;
            }
            let area = ((g2 + eps * eps).sqrt() - eps) * volume;

            let mut own = S::zero();
            let mut sum = S::zero();
            for d in
                [[0i64, 0, 0], [0, 0, 1], [0, 1, 0], [0, 1, 1], [1, 0, 0], [1, 0, 1], [1, 1, 0], [1, 1, 1]]
            {
                let p = [c[0] + d[0], c[1] + d[1], c[2] + d[2]];
                let f = phi(p);
                let w = f * (-f + 1.0) * 4.0 + WEIGHT_FLOOR;
                if p == [0, 0, 0] {
                    own = w;
                }
                sum += w;
            }
            total += area * own / sum;
        }
        total
    }
}

impl LocalResidual for ExchangeKernel {
    fn residual<S: Scalar>(&self, item: usize, current: &[S], _previous: &[S], x: &[S], out: &mut [S]) {
        let s = &self.s;
        let h = s.settings.coefficient(current[2] * s.us);
        let power = h * self.area(item, x) * (current[0] - current[1]) * s.ts;
        out[0] = power / s.scale;
        out[1] = -power / s.scale;
    }
}

impl StepKernel for ExchangeKernel {}

struct DispersionKernel {
    s: Arc<Shared>,
    axes: Vec<usize>,
    nodes: Vec<[usize; 2]>,
}

impl LocalResidual for DispersionKernel {
    fn residual<S: Scalar>(&self, item: usize, current: &[S], _previous: &[S], x: &[S], out: &mut [S]) {
        let s = &self.s;
        let axis = self.axes[item];
        let h = [x[1] * 1e-3, x[2] * 1e-3, x[3] * 1e-3];

        let area = h[0] * h[1] * h[2] / (h[axis] * 4.0);
        let fraction = s.fraction(x[0]);
        let k = s.settings.dispersion_conductivity(current[2] * s.us) * fraction * fraction * fraction;
        let flux = k * area / h[axis] * (current[0] - current[1]) * s.ts;
        out[0] = flux / s.scale;
        out[1] = -flux / s.scale;
    }
}

impl StepKernel for DispersionKernel {
    fn prescribed_state(&self, n: usize, current: &mut [f64], previous: &mut [f64]) {
        let s = &self.s;
        for (e, pair) in self.nodes.iter().enumerate() {
            for (j, node) in pair.iter().enumerate() {
                current[e * 3 + j] = (s.fixed_t[n][*node] - s.t0) / s.ts;
                previous[e * 3 + j] = (s.fixed_t[n.saturating_sub(1)][*node] - s.t0) / s.ts;
            }
        }
    }
}

struct BulkKernel {
    s: Arc<Shared>,
    planes: Vec<usize>,
    length_cells: f64,
    nc: usize,
}

impl BulkKernel {
    fn speed<S: Scalar>(&self, current: &[S], x: &[S]) -> S {
        let mut start = 1;
        let mut flow = S::zero();
        for count in &self.planes {
            for v in &current[start..start + count] {
                flow += *v;
            }
            start += count;
        }
        #[allow(clippy::cast_precision_loss)]
        let planes = self.planes.len() as f64;
        let mut fluid = S::zero();
        for theta in &x[..self.nc] {
            fluid += self.s.fraction(*theta);
        }
        flow * (self.length_cells / planes) / fluid
    }
}

impl LocalResidual for BulkKernel {
    fn residual<S: Scalar>(&self, _item: usize, current: &[S], _previous: &[S], x: &[S], out: &mut [S]) {
        out[0] = current[0] - self.speed(current, x);
    }
}

impl StepKernel for BulkKernel {}

pub struct WallFilm {
    pub settings: FilmSettings,
    pub fluid_rows: std::ops::Range<usize>,
    pub speed_row: usize,
    pub solid_rows: std::ops::Range<usize>,
    shared: Arc<Shared>,
    exchange: Box<dyn GroupOps>,
    dispersion: Option<Box<dyn GroupOps>>,
    bulk: Box<dyn GroupOps>,
    state_size: usize,
    design_size: usize,
    free_t: Vec<usize>,
}

impl std::fmt::Debug for WallFilm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WallFilm").field("settings", &self.settings).finish_non_exhaustive()
    }
}

impl WallFilm {
    #[must_use]
    pub fn block_size(solid: &SolidKernel) -> usize {
        solid.free_t.len() + 1
    }

    #[must_use]
    pub fn initial_state(solid: &SolidKernel) -> Vec<f64> {
        let trs = solid.thermal_row_slice();
        let mut out = solid.initial_state()[trs].to_vec();
        out.push(0.0);
        out
    }


    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn new(
        settings: FilmSettings,
        state_size: usize,
        design_size: usize,
        solid_start: usize,
        fluid_start: usize,
        film_start: usize,
        solid: &SolidKernel,
        fluid: &FluidKernel,
    ) -> CaeResult<Self> {
        if solid.grid != fluid.grid {
            return Err(CaeError::contract("wall_film requires coincident Cartesian grids"));
        }
        let m = &solid.model;
        let grid = solid.grid;
        let nc = solid.nc;
        let n_t = solid.free_t.len();
        let trs = solid.thermal_row_slice();
        let solid_rows = solid_start + trs.start..solid_start + trs.end;
        let fluid_rows = film_start..film_start + n_t;
        let speed_row = film_start + n_t;
        let shared = Arc::new(Shared {
            settings: settings.clone(),
            t0: m.t0,
            ts: m.ts,
            us: fluid.us,
            scale: m.ks * m.ts * m.ls,
            floor: fluid.law.fraction_floor,
            grid,
            fixed_t: solid.fixed_t.clone(),
        });
        let i = |v: usize| i64::try_from(v).unwrap_or(-1);
        let batch = solid.p["assembly"]["batch_size"]
            .as_u64()
            .min(fluid.p["assembly"]["batch_size"].as_u64())
            .and_then(|b| usize::try_from(b).ok())
            .unwrap_or(64)
            .max(1);
        let spacing = [i(nc), i(nc + 1), i(nc + 2)];
        let clamp = |q: [usize; 3], o: [i64; 3]| -> usize {
            let c: [usize; 3] = std::array::from_fn(|a| {
                let v = i64::try_from(q[a]).unwrap_or(0) + o[a];
                let top = i64::try_from(grid[a]).unwrap_or(1) - 1;
                usize::try_from(v.clamp(0, top)).unwrap_or(0)
            });
            flat(grid, c)
        };

        let (mut rows, mut current, mut design, mut nodes) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (k, node) in solid.free_t.iter().enumerate() {
            let q = solid.mesh.ijk[*node];
            rows.push(vec![i(solid_rows.start + k), i(fluid_rows.start + k)]);
            current.push(vec![i(solid_rows.start + k), i(fluid_rows.start + k), i(speed_row)]);
            let mut d = Vec::with_capacity(67);
            for a in -2i64..=1 {
                for b in -2i64..=1 {
                    for c in -2i64..=1 {
                        d.push(i(clamp(q, [a, b, c])));
                    }
                }
            }
            d.extend(spacing);
            design.push(d);
            nodes.push(q);
        }
        let exchange = group(
            ExchangeKernel { s: Arc::clone(&shared), nodes },
            &rows,
            &current,
            &design,
            state_size,
            design_size,
            batch,
        )?
        .ok_or_else(|| CaeError::contract("wall_film requires free temperature nodes"))?;

        let mut tmap = vec![-1i64; solid.nn];
        for (k, node) in solid.free_t.iter().enumerate() {
            tmap[*node] = i(fluid_rows.start + k);
        }
        let node_shape = [grid[0] + 1, grid[1] + 1, grid[2] + 1];
        let mut node_of = vec![usize::MAX; node_shape.iter().product()];
        for (node, q) in solid.mesh.ijk.iter().enumerate() {
            node_of[flat(node_shape, *q)] = node;
        }
        let dispersion = if settings.dispersion > 0.0 {
            let (mut rows, mut current, mut design, mut axes, mut nodes) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for cell in kernel_ndindex(grid) {
                let ci = flat(grid, cell);
                for axis in (0..3).filter(|a| *a != fluid.flow_axis) {
                    let transverse: Vec<usize> = (0..3).filter(|a| *a != axis).collect();
                    for bits in [[0usize, 0usize], [0, 1], [1, 0], [1, 1]] {
                        let mut lo = cell;
                        for (a, bit) in transverse.iter().zip(bits) {
                            lo[*a] += bit;
                        }
                        let mut hi = lo;
                        hi[axis] += 1;
                        let (ni, nj) = (node_of[flat(node_shape, lo)], node_of[flat(node_shape, hi)]);
                        if ni == usize::MAX || nj == usize::MAX {
                            return Err(CaeError::contract("wall_film: inconsistent node numbering"));
                        }
                        rows.push(vec![tmap[ni], tmap[nj]]);
                        current.push(vec![tmap[ni], tmap[nj], i(speed_row)]);
                        design.push(vec![i(ci), spacing[0], spacing[1], spacing[2]]);
                        axes.push(axis);
                        nodes.push([ni, nj]);
                    }
                }
            }
            group(
                DispersionKernel { s: Arc::clone(&shared), axes, nodes },
                &rows,
                &current,
                &design,
                state_size,
                design_size,
                batch,
            )?
        } else {
            None
        };

        let axis = fluid.flow_axis;
        let shape = fluid.map_shapes[axis];
        let mut planes = Vec::new();
        let mut faces = Vec::new();
        for plane in [0, shape[axis] - 1] {
            let mut ids = Vec::new();
            for ix in kernel_ndindex(shape) {
                if ix[axis] != plane {
                    continue;
                }
                let local = fluid.map(axis, ix);
                if local >= 0 {
                    ids.push(i64::try_from(fluid_start).unwrap_or(0) + local);
                }
            }
            if !ids.is_empty() {
                planes.push(ids.len());
                faces.extend(ids);
            }
        }
        if planes.is_empty() {
            return Err(CaeError::contract(
                "wall_film requires a through-flow: no open velocity unknown on the opening planes of the flow axis",
            ));
        }
        let mut current = vec![i(speed_row)];
        current.extend(faces);
        let mut design: Vec<i64> = (0..nc).map(i).collect();
        design.extend(spacing);
        #[allow(clippy::cast_precision_loss)]
        let length_cells = grid[axis] as f64;
        let bulk = group(
            BulkKernel { s: Arc::clone(&shared), planes, length_cells, nc },
            &[vec![i(speed_row)]],
            &[current],
            &[design],
            state_size,
            design_size,
            1,
        )?
        .ok_or_else(|| CaeError::contract("wall_film bulk-speed row is empty"))?;
        Ok(Self {
            settings,
            fluid_rows,
            speed_row,
            solid_rows,
            shared,
            exchange,
            dispersion,
            bulk,
            state_size,
            design_size,
            free_t: solid.free_t.clone(),
        })
    }

    fn groups(&self) -> Vec<&dyn GroupOps> {
        let mut out: Vec<&dyn GroupOps> = vec![self.exchange.as_ref(), self.bulk.as_ref()];
        if let Some(d) = &self.dispersion {
            out.push(d.as_ref());
        }
        out
    }

    #[must_use]
    pub fn bulk_speed(&self, z: &[f64]) -> f64 {
        z[self.speed_row] * self.shared.us
    }

    #[must_use]
    pub fn coefficient(&self, z: &[f64]) -> f64 {
        self.settings.coefficient(self.bulk_speed(z))
    }


    pub fn exchange_powers(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<Vec<(f64, f64)>> {
        let local = self.exchange.local_values(n, z, x)?;
        Ok((0..self.free_t.len())
            .map(|k| {
                let power = local[2 * k] * self.shared.scale;
                let rise = (z[self.solid_rows.start + k] - z[self.fluid_rows.start + k]) * self.shared.ts;
                (power, rise)
            })
            .collect())
    }

    pub fn fluid_nodal_temperature<S: Scalar>(&self, n: usize, full: &[S]) -> Vec<S> {
        let s = &self.shared;
        let mut t: Vec<S> = s.fixed_t[n].iter().map(|v| S::from_f64(*v)).collect();
        for (k, node) in self.free_t.iter().enumerate() {
            t[*node] = full[self.fluid_rows.start + k] * s.ts + s.t0;
        }
        t
    }


    pub fn ledger(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<Value> {
        let rows = self.exchange_powers(n, z, x)?;
        let power: f64 = rows.iter().map(|(p, _)| p).sum();
        let rise = rows.iter().map(|(_, r)| *r).fold(f64::NEG_INFINITY, f64::max);
        let speed = self.bulk_speed(z);
        let coefficient = self.settings.coefficient(speed);

        let nc: usize = self.shared.grid.iter().product();
        let spacing: Vec<f64> = (0..3).map(|a| x.get(nc + a).copied().unwrap_or(f64::NAN) * 1e-3).collect();
        let face = (spacing[0] * spacing[1]).min(spacing[1] * spacing[2]).min(spacing[0] * spacing[2]);
        let (mut area, mut flux_max) = (0.0, 0.0_f64);
        for (p, r) in &rows {
            if r.abs() > 1e-12 && coefficient > 0.0 {
                let a = p / (coefficient * r);
                area += a;
                if a >= WALL_FLUX_AREA_FRACTION * face {
                    flux_max = flux_max.max(coefficient * r);
                }
            }
        }
        Ok(json!({
            "wall_film_exchange_power_W": power,
            "wall_film_max_solid_minus_coolant_K": rise,
            "wall_film_bulk_speed_m_s": speed,
            "wall_film_reynolds": self.settings.reynolds(speed.abs()),
            "wall_film_coefficient_W_m2K": coefficient,
            "wall_film_interface_area_m2": area,
            "wall_film_mean_wall_heat_flux_W_m2": if area > 0.0 { power / area } else { 0.0 },
            "wall_film_max_wall_heat_flux_W_m2": flux_max,
        }))
    }

    #[must_use]
    pub fn report(&self) -> Value {
        let s = &self.settings;
        let mut row = Map::new();
        row.insert("model".into(), json!(MODEL));
        row.insert("hydraulic_diameter_m".into(), json!(s.hydraulic_diameter_m));
        row.insert("enhancement_factor".into(), json!(s.enhancement));
        row.insert("transverse_dispersion_coefficient".into(), json!(s.dispersion));
        row.insert("prandtl".into(), json!(s.prandtl()));
        row.insert("nusselt".into(), json!("3.66 (Re <= 2300); Gnielinski 1976 with Petukhov friction (Re >= 1e4); linear transition (VDI Heat Atlas G1)"));
        row.insert(
            "interface_area".into(),
            json!("isotropic diffuse |grad theta| per cell, distributed to nodes by 4 phi (1 - phi)"),
        );
        row.insert("heat_path_split".into(), json!("solid operator on T_s; coolant operators on T_f; film conductance is the only solid-coolant path; transverse dispersion mixes the coolant cross-section"));
        row.insert(
            "exact_partials".into(),
            json!(["current_state", "solid_fraction", "cell_spacing", "bulk_speed"]),
        );
        row.insert("provenance".into(), json!(s.provenance));
        Value::Object(row)
    }
}

impl HistoryInterface for WallFilm {
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        let mut out = vec![0.0; self.state_size];
        for g in self.groups() {
            for (o, v) in out.iter_mut().zip(g.residual(n, z, old, x)?) {
                *o += v;
            }
        }
        Ok(out)
    }

    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        let cols = if kind == Kind::Design { self.design_size } else { self.state_size };
        Ok(Jacobian::Csr(sum_jacobians(self.groups(), kind, n, z, old, x, (self.state_size, cols))?))
    }

    fn current_action(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        v: &[f64],
        transpose: bool,
    ) -> Option<CaeResult<Vec<f64>>> {
        Some((|| {
            let mut out = vec![0.0; self.state_size];
            for g in self.groups() {
                for (o, a) in out.iter_mut().zip(g.current_action(n, z, old, x, v, transpose)?) {
                    *o += a;
                }
            }
            Ok(out)
        })())
    }
}

