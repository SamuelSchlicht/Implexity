// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::py_repr::repr_float;
use implexity_core::pyobj::repr;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_physics_base::boundary_regions::{closed_edge_fraction, pressure_cell};
use implexity_physics_base::material_domains::interval_status;
use implexity_physics_cfd::pyfmt::fmt_g;
use implexity_physics_solid::phase_stress_transfer::MacFluid;
use implexity_solve::local_assembly::Kind;

use super::groups::{
    AdvectionKernel, BoundaryThermalKernel, CellKernel, CellPart, InteriorThermalKernel, MacParams,
    MixingLength, OpenPressureKernel, OpeningAdvectionKernel, ShearItem, ShearKernel, ShearPart,
};
use super::material::{FluidCard, FluidLaw};
use super::{
    BRANCH_CONTRACT, KERNEL_LIMITATIONS, MASS_BALANCE_RELATIVE_TOLERANCE, normalise_fluid, selected_material,
};
use crate::local_group::{GroupOps, Role, group, sum_jacobians};

pub const SHARED_TEMPERATURE_CALLBACKS: [&str; 8] = [
    "retained_R",
    "retained_A",
    "retained_B",
    "retained_C",
    "dissipation_R",
    "dissipation_A",
    "dissipation_B",
    "dissipation_C",
];

#[must_use]
pub fn shared_temperature_energy_contract() -> Value {
    json!({
        "schema": "implexity-shared-temperature-fluid-energy/1",
        "profile": super::SHARED_TEMPERATURE_ENERGY_PROFILE,
        "thermal_row_slice": "nv_plus_nc_to_state_size",
        "retained_residual": "mechanics_continuity_and_face_boundary_advection_conduction",
        "caloric_owner": "shared_temperature_parity_kuhn_T4_row_sum_nodal_dual_volume",
        "dissipation_ledger": "native_integrated_cell_normal_brinkman_and_edge_shear_power",
        "dissipation_sign": "negative_heat_source_in_energy_residual",
        "dissipation_scale": "physical_power_W_divided_by_fluid_power_scale_W",
        "exact_partials": ["current_state", "previous_state", "design"],
        "coefficient_sampling": "native_cell_and_edge_Q_temperature_mu_alpha_velocity",
        "standalone_default": super::STANDALONE_ENERGY_PROFILE,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnergyProfile {
    Standalone,
    SharedTemperature,
}

#[derive(Debug, Clone)]
pub struct ShearRecord {
    pub a: usize,
    pub b: usize,
    pub edge: [usize; 3],
    pub iv: [i64; 4],
    pub ci: [usize; 4],
    pub w: [f64; 4],
    pub factors: [f64; 2],
    pub vf: f64,
}

#[derive(Debug, Clone)]
pub struct ThermalBoundaryRecord {
    pub axis: usize,
    pub side: String,
    pub boundary: Value,
    pub cells: Vec<[usize; 3]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FluidFaces {
    pub faces: [Vec<f64>; 3],
    pub pressure: Vec<f64>,
    pub temperature: Vec<f64>,
    pub velocity: Vec<[f64; 3]>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FluidMetrics {
    pub flow_in: f64,
    pub flow_out: f64,
    pub hydraulic_power: f64,
    pub enthalpy: f64,
    pub body_force_power: f64,
    pub temperature_peak: f64,
    pub pressure_absolute_min: f64,
}

impl FluidMetrics {
    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({"flow_in_m3_s": self.flow_in, "flow_out_m3_s": self.flow_out,
            "hydraulic_power_W": self.hydraulic_power, "enthalpy_J": self.enthalpy,
            "body_force_power_W": self.body_force_power, "temperature_peak_K": self.temperature_peak,
            "pressure_absolute_min_Pa": self.pressure_absolute_min})
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodalFluidLaw {
    pub law: FluidLaw,
    pub density: f64,
}

#[derive(Debug, Clone)]
pub struct FluidBoundary {
    pub axis: usize,
    pub side: String,
    pub pressure: bool,
    pub thermal: String,
    pub raw: Value,
}

impl FluidBoundary {
    fn history(&self, key: &str) -> Option<Vec<f64>> {
        self.raw.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_f64).collect())
    }
}

type GroupEntry = (Box<dyn GroupOps>, Role);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupSet {
    All,
    Retained,
    Dissipation,
    Caloric,
    Mechanical,
}

pub struct FluidKernel {
    pub p: Value,
    pub grid: [usize; 3],
    pub nc: usize,
    pub nt: usize,
    pub card: FluidCard,
    pub law: FluidLaw,
    pub inactive_phase_numerical_material: Option<Value>,
    pub boundaries: Vec<FluidBoundary>,
    pub flow_axis: usize,
    pub us: f64,
    pub ps: f64,
    pub ts: f64,
    pub ls: f64,
    pub fs: f64,
    pub ms: f64,
    pub hs: f64,
    pub t0: f64,
    pub pref: f64,
    pub rho: f64,
    pub maps: [Vec<i64>; 3],
    pub map_shapes: [[usize; 3]; 3],
    pub velocity_records: Vec<(usize, [usize; 3])>,
    pub nv: usize,
    pub state_size: usize,
    pub design_size: usize,
    pub profile: EnergyProfile,
    pub params: Arc<MacParams>,
    pub shear_records: Vec<ShearRecord>,
    pub thermal_boundary_records: Vec<ThermalBoundaryRecord>,
    pub advection_energy_interior: Option<(Vec<[i64; 4]>, Vec<(usize, f64)>)>,
    pub advection_energy_boundary: Vec<(usize, f64, Vec<[i64; 3]>, Vec<f64>)>,
    groups: Vec<GroupEntry>,
    mechanical: Vec<usize>,
}

impl std::fmt::Debug for FluidKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FluidKernel")
            .field("grid", &self.grid)
            .field("nv", &self.nv)
            .field("state_size", &self.state_size)
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}

pub fn ndindex(shape: [usize; 3]) -> impl Iterator<Item = [usize; 3]> {
    (0..shape[0]).flat_map(move |i| (0..shape[1]).flat_map(move |j| (0..shape[2]).map(move |k| [i, j, k])))
}

#[must_use]
pub fn flat(shape: [usize; 3], ix: [usize; 3]) -> usize {
    (ix[0] * shape[1] + ix[1]) * shape[2] + ix[2]
}

fn turbulence_of(p: &Value) -> Option<MixingLength> {
    let t = p.get("turbulence")?;
    Some(MixingLength { length_m: t["mixing_length_m"].as_f64()?, rate_floor_s: t["rate_floor_s"].as_f64()? })
}

fn side_name(lo: bool) -> &'static str {
    if lo { "lo" } else { "hi" }
}

fn usize_of(v: &Value) -> usize {
    v.as_u64().and_then(|a| usize::try_from(a).ok()).unwrap_or(0)
}

fn py_tuple(ix: [usize; 3]) -> String {
    format!("({}, {}, {})", ix[0], ix[1], ix[2])
}

impl FluidKernel {

    #[allow(clippy::too_many_lines)]
    pub fn new(problem: &Value, profile: EnergyProfile) -> CaeResult<Self> {
        let p = normalise_fluid(problem)?;
        let grid: [usize; 3] = std::array::from_fn(|a| usize_of(&p["grid"][a]));
        let nc = grid.iter().product();
        let times: Vec<f64> =
            p["times_s"].as_array().map(|t| t.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
        let nt = times.len();
        let component = p["material_component"].as_str().unwrap_or_default();
        selected_material(component)?;
        let card = FluidCard::from_value(&p["material"])?;
        let policy = p.get("inactive_phase_numerical_material").cloned();
        let floor = p["regularisation"]["fluid_fraction_floor"].as_f64().unwrap_or(0.0);
        let law = FluidLaw { card: card.clone(), policy: policy.clone(), fraction_floor: floor };
        let boundaries: Vec<FluidBoundary> = p["boundaries"]
            .as_array()
            .map(|bs| {
                bs.iter()
                    .map(|b| FluidBoundary {
                        axis: usize_of(&b["axis"]),
                        side: b["side"].as_str().unwrap_or_default().to_string(),
                        pressure: b["momentum"].as_str() == Some("pressure"),
                        thermal: b["thermal"].as_str().unwrap_or_default().to_string(),
                        raw: b.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let flow_axis = boundaries.iter().find(|b| b.pressure).map_or(0, |b| b.axis);
        let n = &p["numerics"];
        let num = |k: &str| n[k].as_f64().unwrap_or(f64::NAN);
        let (us, ps, ts, ls, hs) = (
            num("velocity_scale_m_s"),
            num("pressure_scale_Pa"),
            num("temperature_scale_K"),
            num("length_scale_m"),
            num("power_scale_W"),
        );
        let fs = ps * ls * ls;
        let ms = us * ls * ls;
        let t0 = p["initial_temperature_K"].as_f64().unwrap_or(f64::NAN);
        let pref = p["pressure_reference_Pa"].as_f64().unwrap_or(f64::NAN);
        let rho = card.density;
        let body = p.get("body_acceleration").map(|b| {
            let g: [f64; 3] = std::array::from_fn(|a| b["acceleration_m_s2"][a].as_f64().unwrap_or(0.0));
            (
                g,
                b["thermal_expansion_K_inv"].as_f64().unwrap_or(0.0),
                b["reference_temperature_K"].as_f64().unwrap_or(0.0),
            )
        });
        let params = Arc::new(MacParams {
            law: law.clone(),
            rho,
            us,
            ps,
            ts,
            fs,
            ms,
            hs,
            t0,
            pref,
            brinkman_max: p["regularisation"]["brinkman_max_Pa_s_m2"].as_f64().unwrap_or(0.0),
            exponent: p["regularisation"]["obstruction_exponent"].as_f64().unwrap_or(1.0),
            body,
            times: times.clone(),
            heat: p["volumetric_heat_W_m3"]
                .as_array()
                .map(|q| q.iter().filter_map(Value::as_f64).collect())
                .unwrap_or_default(),
            turbulence: turbulence_of(&p),
            advection_smoothing: p.get(super::ADVECTION_SMOOTHING_KEY).and_then(Value::as_f64),
        });
        let mut kernel = Self {
            p,
            grid,
            nc,
            nt,
            card,
            law,
            inactive_phase_numerical_material: policy,
            boundaries,
            flow_axis,
            us,
            ps,
            ts,
            ls,
            fs,
            ms,
            hs,
            t0,
            pref,
            rho,
            maps: [Vec::new(), Vec::new(), Vec::new()],
            map_shapes: [[0; 3]; 3],
            velocity_records: Vec::new(),
            nv: 0,
            state_size: 0,
            design_size: nc + 3,
            profile,
            params,
            shear_records: Vec::new(),
            thermal_boundary_records: Vec::new(),
            advection_energy_interior: None,
            advection_energy_boundary: Vec::new(),
            groups: Vec::new(),
            mechanical: Vec::new(),
        };
        let mut counter: i64 = 0;
        for axis in 0..3 {
            let mut shape = grid;
            shape[axis] += 1;
            let mut arr = vec![-1i64; shape.iter().product()];
            for ix in ndindex(shape) {
                let side = if ix[axis] == 0 {
                    Some(true)
                } else if ix[axis] == grid[axis] {
                    Some(false)
                } else {
                    None
                };
                if let Some(lo) = side {
                    let mut cell = ix;
                    cell[axis] = if lo { 0 } else { grid[axis] - 1 };
                    if !kernel.is_pressure_cell(axis, side_name(lo), cell)? {
                        continue;
                    }
                }
                arr[flat(shape, ix)] = counter;
                kernel.velocity_records.push((axis, ix));
                counter += 1;
            }
            kernel.maps[axis] = arr;
            kernel.map_shapes[axis] = shape;
        }
        kernel.nv = usize::try_from(counter).unwrap_or(0);
        kernel.state_size = kernel.nv + 2 * nc;
        kernel.build_cells()?;
        kernel.build_shear()?;
        kernel.build_thermal_faces()?;
        kernel.build_open_pressure()?;
        if kernel.p["momentum_advection"].as_bool() == Some(true) {
            kernel.build_advection()?;
        }
        if profile == EnergyProfile::SharedTemperature {
            let cutoff = i64::try_from(kernel.nv + nc).unwrap_or(i64::MAX);
            for (index, (g, role)) in kernel.groups.iter().enumerate() {
                if *role != Role::Retained {
                    continue;
                }
                let active: Vec<i64> = g.rows().values.iter().copied().filter(|r| *r >= 0).collect();
                let below = active.iter().any(|r| *r < cutoff);
                let above = active.iter().any(|r| *r >= cutoff);
                if below && above {
                    return Err(CaeError::contract(
                        "nodal transport requires disjoint retained mechanical/thermal groups",
                    ));
                }
                if !active.is_empty() && !above {
                    kernel.mechanical.push(index);
                }
            }
        }
        Ok(kernel)
    }

    #[must_use]
    pub fn map(&self, axis: usize, face: [usize; 3]) -> i64 {
        self.maps[axis][flat(self.map_shapes[axis], face)]
    }

    #[must_use]
    pub fn cell_index(&self, cell: [usize; 3]) -> usize {
        flat(self.grid, cell)
    }

    #[must_use]
    pub fn pid(&self, cell: [usize; 3]) -> usize {
        self.nv + self.cell_index(cell)
    }

    #[must_use]
    pub fn tid(&self, cell: [usize; 3]) -> usize {
        self.nv + self.nc + self.cell_index(cell)
    }

    #[must_use]
    pub fn boundary(&self, axis: usize, side: &str) -> Option<&FluidBoundary> {
        self.boundaries.iter().find(|b| b.axis == axis && b.side == side)
    }

    fn batch(&self) -> usize {
        usize_of(&self.p["assembly"]["batch_size"]).max(1)
    }

    fn push(&mut self, group: Option<Box<dyn GroupOps>>, role: Role) -> CaeResult<()> {
        if let Some(g) = group {
            if self.profile == EnergyProfile::Standalone && role != Role::Retained {
                return Err(CaeError::contract(
                    "shared fluid residual roles require the private shared-energy factory path",
                ));
            }
            self.groups.push((g, role));
        }
        Ok(())
    }

    fn i(v: usize) -> i64 {
        i64::try_from(v).unwrap_or(-1)
    }

    fn build_cells(&mut self) -> CaeResult<()> {
        let mut rows = Vec::new();
        let mut design = Vec::new();
        let nc = self.nc;
        for cell in ndindex(self.grid) {
            let mut faces = Vec::with_capacity(8);
            for a in 0..3 {
                let mut plus = cell;
                plus[a] += 1;
                faces.push(self.map(a, cell));
                faces.push(self.map(a, plus));
            }
            faces.push(Self::i(self.pid(cell)));
            faces.push(Self::i(self.tid(cell)));
            rows.push(faces);
            design.push(vec![Self::i(self.cell_index(cell)), Self::i(nc), Self::i(nc + 1), Self::i(nc + 2)]);
        }
        let (ss, ds, batch) = (self.state_size, self.design_size, self.batch());
        let params = Arc::clone(&self.params);
        let kernel = |part| CellKernel { p: Arc::clone(&params), part, count: nc };
        if self.profile == EnergyProfile::Standalone {
            let g = group(kernel(CellPart::Standalone), &rows, &rows, &design, ss, ds, batch)?;
            return self.push(g, Role::Retained);
        }
        let mech_rows: Vec<Vec<i64>> = rows.iter().map(|r| r[..7].to_vec()).collect();
        let t_rows: Vec<Vec<i64>> = rows.iter().map(|r| vec![r[7]]).collect();
        let g = group(kernel(CellPart::Mechanics), &mech_rows, &rows, &design, ss, ds, batch)?;
        self.push(g, Role::Retained)?;
        let g = group(kernel(CellPart::Caloric), &t_rows, &rows, &design, ss, ds, batch)?;
        self.push(g, Role::Caloric)?;
        let g = group(kernel(CellPart::Dissipation), &t_rows, &rows, &design, ss, ds, batch)?;
        self.push(g, Role::Dissipation)
    }

    fn build_shear(&mut self) -> CaeResult<()> {
        let (mut rows, mut design, mut items) = (Vec::new(), Vec::new(), Vec::new());
        let mut records = Vec::new();
        let g = self.grid;
        let gi = |a: usize| i64::try_from(g[a]).unwrap_or(0);
        for (a, b) in [(0usize, 1usize), (0, 2), (1, 2)] {
            let mut shape = g;
            shape[a] += 1;
            shape[b] += 1;
            for edge in ndindex(shape) {
                let mut wall_weight = 1.0;
                for axis in [a, b] {
                    let side = if edge[axis] == 0 {
                        Some("lo")
                    } else if edge[axis] == g[axis] {
                        Some("hi")
                    } else {
                        None
                    };
                    if let Some(side) = side
                        && let Some(bd) = self.boundary(axis, side)
                        && bd.pressure
                    {
                        let other = if axis == a { b } else { a };
                        let e = edge.map(|v| i64::try_from(v).unwrap_or(0));
                        wall_weight *= closed_edge_fraction(&bd.raw, g, e, other)?;
                    }
                }
                if wall_weight == 0.0 {
                    continue;
                }
                let mut iv = [0i64; 4];
                let mut factors = [0.0; 2];
                for (k, (component, direction)) in [(a, b), (b, a)].into_iter().enumerate() {
                    for (j, delta) in [-1i64, 0].into_iter().enumerate() {
                        let pos = i64::try_from(edge[direction]).unwrap_or(0) + delta;
                        iv[2 * k + j] = if pos >= 0 && pos < gi(direction) {
                            let mut ix = edge;
                            ix[direction] = usize::try_from(pos).unwrap_or(0);
                            self.map(component, ix)
                        } else {
                            -1
                        };
                    }
                    factors[k] =
                        if edge[direction] == 0 || edge[direction] == g[direction] { 2.0 } else { 1.0 };
                }
                let mut cells = Vec::new();
                for da in [-1i64, 0] {
                    for db in [-1i64, 0] {
                        let pa = i64::try_from(edge[a]).unwrap_or(0) + da;
                        let pb = i64::try_from(edge[b]).unwrap_or(0) + db;
                        if pa >= 0 && pa < gi(a) && pb >= 0 && pb < gi(b) {
                            let mut ix = edge;
                            ix[a] = usize::try_from(pa).unwrap_or(0);
                            ix[b] = usize::try_from(pb).unwrap_or(0);
                            cells.push(self.cell_index(ix));
                        }
                    }
                }
                let mut w = [0.0; 4];
                let count = cells.len();
                for slot in w.iter_mut().take(count) {
                    *slot = 1.0 / count as f64;
                }
                let last = *cells.last().unwrap_or(&0);
                let ci: [usize; 4] = std::array::from_fn(|j| if j < count { cells[j] } else { last });
                let mut ids: Vec<i64> = iv.to_vec();
                ids.extend(ci.iter().map(|c| Self::i(self.nv + self.nc + c)));
                let volume_factor = wall_weight
                    * (if edge[a] == 0 || edge[a] == g[a] { 0.5 } else { 1.0 })
                    * (if edge[b] == 0 || edge[b] == g[b] { 0.5 } else { 1.0 });
                rows.push(ids);
                let mut inputs = vec![Self::i(self.nc), Self::i(self.nc + 1), Self::i(self.nc + 2)];
                if self.params.turbulence.is_some() {
                    inputs.extend(ci.iter().map(|c| Self::i(*c)));
                }
                design.push(inputs);
                items.push(ShearItem { a, b, fa: factors[0], fb: factors[1], vf: volume_factor, weights: w });
                records.push(ShearRecord { a, b, edge, iv, ci, w, factors, vf: volume_factor });
            }
        }
        self.shear_records = records;
        if rows.is_empty() {
            return Ok(());
        }
        let items = Arc::new(items);
        let (ss, ds, batch) = (self.state_size, self.design_size, self.batch());
        let params = Arc::clone(&self.params);
        let kernel = |part| ShearKernel { p: Arc::clone(&params), items: Arc::clone(&items), part };
        if self.profile == EnergyProfile::Standalone {
            let g = group(kernel(ShearPart::Standalone), &rows, &rows, &design, ss, ds, batch)?;
            return self.push(g, Role::Retained);
        }
        let force_rows: Vec<Vec<i64>> = rows.iter().map(|r| r[..4].to_vec()).collect();
        let heat_rows: Vec<Vec<i64>> = rows.iter().map(|r| r[4..].to_vec()).collect();
        let g = group(kernel(ShearPart::Mechanics), &force_rows, &rows, &design, ss, ds, batch)?;
        self.push(g, Role::Retained)?;
        let g = group(kernel(ShearPart::Dissipation), &heat_rows, &rows, &design, ss, ds, batch)?;
        self.push(g, Role::Dissipation)
    }

    fn build_thermal_faces(&mut self) -> CaeResult<()> {
        let (mut rows, mut current, mut design, mut axes) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let nc = self.nc;
        for a in 0..3 {
            for cell in ndindex(self.grid) {
                if cell[a] + 1 >= self.grid[a] {
                    continue;
                }
                let mut plus = cell;
                plus[a] += 1;
                let (i, j) = (self.cell_index(cell), self.cell_index(plus));
                rows.push(vec![Self::i(self.tid(cell)), Self::i(self.tid(plus))]);
                current.push(vec![Self::i(self.tid(cell)), Self::i(self.tid(plus)), self.map(a, plus)]);
                design.push(vec![Self::i(nc), Self::i(nc + 1), Self::i(nc + 2), Self::i(i), Self::i(j)]);
                axes.push(a);
            }
        }
        let (ss, ds, batch) = (self.state_size, self.design_size, self.batch());
        if !rows.is_empty() {
            let kernel = InteriorThermalKernel { p: Arc::clone(&self.params), axes: Arc::new(axes) };
            let g = group(kernel, &rows, &current, &design, ss, ds, batch)?;
            self.push(g, Role::Retained)?;
        }
        let mut records = Vec::new();
        for b in self.boundaries.clone() {
            if b.thermal == "interface" {
                continue;
            }
            let lo = b.side == "lo";
            let cells = self.face_cells(b.axis, &b.side);
            let (mut rr, mut cc, mut xx) = (Vec::new(), Vec::new(), Vec::new());
            for cell in &cells {
                let mut face = *cell;
                if !lo {
                    face[b.axis] += 1;
                }
                rr.push(vec![Self::i(self.tid(*cell))]);
                cc.push(vec![Self::i(self.tid(*cell)), self.map(b.axis, face)]);
                xx.push(vec![Self::i(nc), Self::i(nc + 1), Self::i(nc + 2), Self::i(self.cell_index(*cell))]);
            }
            let kernel = BoundaryThermalKernel {
                p: Arc::clone(&self.params),
                axis: b.axis,
                sign: if lo { -1.0 } else { 1.0 },
                open: b.pressure,
                temperature: b.thermal == "temperature",
                incoming: b.history("incoming_temperature_K").unwrap_or_else(|| vec![self.t0; self.nt]),
                tb: b.history("temperature_K").unwrap_or_else(|| vec![self.t0; self.nt]),
                count: rr.len(),
            };
            let g = group(kernel, &rr, &cc, &xx, ss, ds, batch)?;
            self.push(g, Role::Retained)?;
            records.push(ThermalBoundaryRecord {
                axis: b.axis,
                side: b.side.clone(),
                boundary: b.raw.clone(),
                cells,
            });
        }
        self.thermal_boundary_records = records;
        Ok(())
    }

    fn build_open_pressure(&mut self) -> CaeResult<()> {
        let (ss, ds, batch, nc) = (self.state_size, self.design_size, self.batch(), self.nc);
        for b in self.boundaries.clone() {
            if !b.pressure {
                continue;
            }
            let lo = b.side == "lo";
            let mut rows = Vec::new();
            for cell in self.opening_cells(b.axis, &b.side)? {
                let mut ix = cell;
                if !lo {
                    ix[b.axis] += 1;
                }
                rows.push(vec![self.map(b.axis, ix)]);
            }
            let design: Vec<Vec<i64>> =
                rows.iter().map(|_| vec![Self::i(nc), Self::i(nc + 1), Self::i(nc + 2)]).collect();
            let kernel = OpenPressureKernel {
                p: Arc::clone(&self.params),
                axis: b.axis,
                sign: if lo { -1.0 } else { 1.0 },
                pressure: b.history("pressure_absolute_Pa").unwrap_or_default(),
                count: rows.len(),
            };
            let g = group(kernel, &rows, &rows, &design, ss, ds, batch)?;
            self.push(g, Role::Retained)?;
        }
        Ok(())
    }

    fn advecting(
        &self,
        transport_axis: usize,
        ix: [usize; 3],
        direction: usize,
        face_index: usize,
    ) -> [i64; 2] {
        if direction == transport_axis {
            let mut b = ix;
            b[direction] = face_index;
            return [self.map(direction, ix), self.map(direction, b)];
        }
        let mut ids = [0i64; 2];
        for (k, da) in [-1i64, 0].into_iter().enumerate() {
            let mut q = ix;
            q[direction] = face_index;
            let pos = (i64::try_from(q[transport_axis]).unwrap_or(0) + da)
                .clamp(0, i64::try_from(self.grid[transport_axis]).unwrap_or(1) - 1);
            q[transport_axis] = usize::try_from(pos).unwrap_or(0);
            ids[k] = self.map(direction, q);
        }
        ids
    }

    fn build_advection(&mut self) -> CaeResult<()> {
        let (ss, ds, batch, nc) = (self.state_size, self.design_size, self.batch(), self.nc);
        let spacing_design = || vec![Self::i(nc), Self::i(nc + 1), Self::i(nc + 2)];
        let (mut rows, mut current, mut meta) = (Vec::new(), Vec::new(), Vec::new());
        let mut energy_ids = Vec::new();
        for a in 0..3 {
            let shape = self.map_shapes[a];
            for ix in ndindex(shape) {
                for b in 0..3 {
                    let mut right = ix;
                    right[b] += 1;
                    if right[b] >= shape[b] {
                        continue;
                    }
                    let (i, j) = (self.map(a, ix), self.map(a, right));
                    if i < 0 && j < 0 {
                        continue;
                    }
                    let adv = if a == b { [i, j] } else { self.advecting(a, ix, b, right[b]) };
                    let factor = if a != b && (ix[a] == 0 || ix[a] == self.grid[a]) { 0.5 } else { 1.0 };
                    rows.push(vec![i, j]);
                    current.push(vec![i, j, adv[0], adv[1]]);
                    energy_ids.push([i, j, adv[0], adv[1]]);
                    meta.push((b, factor));
                }
            }
        }
        self.advection_energy_interior = Some((energy_ids, meta.clone()));
        if !rows.is_empty() {
            let design: Vec<Vec<i64>> = rows.iter().map(|_| spacing_design()).collect();
            let kernel = AdvectionKernel { p: Arc::clone(&self.params), meta: Arc::new(meta) };
            let g = group(kernel, &rows, &current, &design, ss, ds, batch)?;
            self.push(g, Role::Retained)?;
        }
        let mut boundary_records = Vec::new();
        for bc in self.boundaries.clone() {
            if !bc.pressure {
                continue;
            }
            let b = bc.axis;
            let lo = bc.side == "lo";
            let (mut rows, mut cur, mut factors) = (Vec::new(), Vec::new(), Vec::new());
            for a in 0..3 {
                let shape = self.map_shapes[a];
                for ix in ndindex(shape) {
                    let id = self.map(a, ix);
                    if id < 0 {
                        continue;
                    }
                    let target = if lo { 0 } else { shape[b] - 1 };
                    if ix[b] != target {
                        continue;
                    }
                    let ids = if a == b {
                        [id, id]
                    } else {
                        self.advecting(a, ix, b, if lo { 0 } else { self.grid[b] })
                    };
                    rows.push(vec![id]);
                    cur.push(vec![id, ids[0], ids[1]]);
                    factors.push(if a != b && (ix[a] == 0 || ix[a] == self.grid[a]) { 0.5 } else { 1.0 });
                }
            }
            let sign = if lo { -1.0 } else { 1.0 };
            boundary_records.push((
                b,
                sign,
                cur.iter().map(|c| [c[0], c[1], c[2]]).collect(),
                factors.clone(),
            ));
            if !rows.is_empty() {
                let design: Vec<Vec<i64>> = rows.iter().map(|_| spacing_design()).collect();
                let kernel = OpeningAdvectionKernel {
                    p: Arc::clone(&self.params),
                    axis: b,
                    sign,
                    factors: Arc::new(factors),
                };
                let g = group(kernel, &rows, &cur, &design, ss, ds, batch)?;
                self.push(g, Role::Retained)?;
            }
        }
        self.advection_energy_boundary = boundary_records;
        Ok(())
    }

    fn selected(&self, set: GroupSet) -> Vec<&dyn GroupOps> {
        match set {
            GroupSet::All => self.groups.iter().map(|(g, _)| g.as_ref()).collect(),
            GroupSet::Retained => {
                self.groups.iter().filter(|(_, r)| *r == Role::Retained).map(|(g, _)| g.as_ref()).collect()
            }
            GroupSet::Caloric => {
                self.groups.iter().filter(|(_, r)| *r == Role::Caloric).map(|(g, _)| g.as_ref()).collect()
            }
            GroupSet::Dissipation => {
                self.groups.iter().filter(|(_, r)| *r == Role::Dissipation).map(|(g, _)| g.as_ref()).collect()
            }
            GroupSet::Mechanical => self.mechanical.iter().map(|i| self.groups[*i].0.as_ref()).collect(),
        }
    }

    fn require_shared(&self, set: GroupSet) -> CaeResult<()> {
        if set != GroupSet::All && self.profile != EnergyProfile::SharedTemperature {
            return Err(CaeError::contract(
                "shared fluid residual roles require the private shared-energy factory path",
            ));
        }
        Ok(())
    }


    pub fn residual(
        &self,
        set: GroupSet,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
    ) -> CaeResult<Vec<f64>> {
        self.require_shared(set)?;
        let mut out = vec![0.0; self.state_size];
        for g in self.selected(set) {
            for (o, v) in out.iter_mut().zip(g.residual(n, z, old, x)?) {
                *o += v;
            }
        }
        Ok(out)
    }


    pub fn jacobian(
        &self,
        set: GroupSet,
        kind: Kind,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
    ) -> CaeResult<CsrMatrix> {
        self.require_shared(set)?;
        let cols = if kind == Kind::Design { self.design_size } else { self.state_size };
        sum_jacobians(self.selected(set), kind, n, z, old, x, (self.state_size, cols))
    }


    #[allow(clippy::too_many_arguments)]
    pub fn current_action(
        &self,
        set: GroupSet,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        v: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        self.require_shared(set)?;
        let mut out = vec![0.0; self.state_size];
        for g in self.selected(set) {
            for (o, a) in out.iter_mut().zip(g.current_action(n, z, old, x, v, transpose)?) {
                *o += a;
            }
        }
        Ok(out)
    }

    #[must_use]
    pub fn group_reports(&self, set: GroupSet) -> Vec<Value> {
        self.selected(set).iter().map(|g| g.report()).collect()
    }

    #[must_use]
    pub fn template_capacity(&self, kinds: &[Kind]) -> usize {
        self.groups.iter().map(|(g, _)| g.template_capacity(kinds)).sum()
    }


    pub fn nodal_constitutive_law(&self) -> CaeResult<NodalFluidLaw> {
        if self.profile != EnergyProfile::SharedTemperature {
            return Err(CaeError::contract("nodal thermal contract requires the shared-energy profile"));
        }
        Ok(NodalFluidLaw { law: self.law.clone(), density: self.rho })
    }

    #[must_use]
    pub fn nodal_transport_capability(&self) -> Option<&'static str> {
        (self.profile == EnergyProfile::SharedTemperature).then_some(crate::nodal_transport::CAPABILITY)
    }

    #[must_use]
    pub fn face_cells(&self, axis: usize, side: &str) -> Vec<[usize; 3]> {
        let target = if side == "lo" { 0 } else { self.grid[axis] - 1 };
        ndindex(self.grid).filter(|ix| ix[axis] == target).collect()
    }


    pub fn is_pressure_cell(&self, axis: usize, side: &str, cell: [usize; 3]) -> CaeResult<bool> {
        let Some(b) = self.boundary(axis, side) else { return Ok(false) };
        if b.raw.get("opening").is_none() {
            return Ok(b.pressure);
        }
        pressure_cell(&b.raw, self.grid, cell)
    }


    pub fn opening_cells(&self, axis: usize, side: &str) -> CaeResult<Vec<[usize; 3]>> {
        let mut out = Vec::new();
        for c in self.face_cells(axis, side) {
            if self.is_pressure_cell(axis, side, c)? {
                out.push(c);
            }
        }
        Ok(out)
    }


    pub fn pressure_opening_support(&self, x: &[f64]) -> CaeResult<Value> {
        if x.len() != self.nc + 3
            || x.iter().any(|v| !v.is_finite())
            || x[..self.nc].iter().any(|v| *v < 0.0 || *v > 1.0)
            || x[self.nc..].iter().any(|v| *v <= 0.0)
        {
            return Err(CaeError::contract(
                "pressure-opening support requires the complete valid fluid design",
            ));
        }
        let h: [f64; 3] = std::array::from_fn(|a| x[self.nc + a] * 1e-3);
        let mut faces: Vec<&FluidBoundary> = self.boundaries.iter().collect();
        faces.sort_by(|a, b| (a.axis, a.side.as_str()).cmp(&(b.axis, b.side.as_str())));
        let mut rows = Vec::new();
        for b in faces {
            if !b.pressure {
                continue;
            }
            let cells = self.opening_cells(b.axis, &b.side)?;
            let rho: Vec<f64> = cells.iter().map(|c| x[self.cell_index(*c)]).collect();
            let area = h[0] * h[1] * h[2] / h[b.axis];
            let opening = b.raw.get("opening");
            rows.push(json!({"axis": b.axis, "side": b.side, "boundary_cell_count": cells.len(),
                "cell_face_area_m2": area, "pressure_bc_area_m2": area * cells.len() as f64,
                "physical_fluid_weighted_area_m2": area * rho.iter().map(|r| 1.0 - r).sum::<f64>(),
                "solid_weighted_area_m2": area * rho.iter().sum::<f64>(),
                "exact_void_cell_count": rho.iter().filter(|r| **r == 0.0).count(),
                "exact_solid_cell_count": rho.iter().filter(|r| **r == 1.0).count(),
                "intermediate_cell_count": rho.iter().filter(|r| **r > 0.0 && **r < 1.0).count(),
                "pressure_bc_intersects_positive_solid_fraction": rho.iter().any(|r| *r > 0.0),
                "boundary_condition_support": if opening.is_some() { "explicit_grid_aligned_rectangle" } else { "entire_cartesian_boundary_face" },
                "opening_id": opening.and_then(|o| o.get("id")).cloned().unwrap_or(Value::Null),
                "closed_boundary_cell_count": self.face_cells(b.axis, &b.side).len() - cells.len(),
                "closed_remainder": opening.and_then(|o| o.get("closed_remainder")).cloned().unwrap_or(Value::Null),
                "geometry_masks_boundary_condition": false}));
        }
        Ok(
            json!({"schema": "implexity-pressure-opening-support/1", "diagnostic_only": true, "available": true,
            "boundaries": rows, "finite_resolution_geometric_estimate": true,
            "exterior_manifold_and_connection_losses_resolved": false,
            "interpretation": "Cell-face occupancy measures; not an independently resolved port aperture, flow rate, or pressure loss."}),
        )
    }

    #[must_use]
    pub fn initial_state(&self) -> Vec<f64> {
        let mut z = vec![0.0; self.state_size];
        let p0 = self
            .boundary(self.flow_axis, "lo")
            .and_then(|b| b.history("pressure_absolute_Pa"))
            .and_then(|h| h.first().copied())
            .unwrap_or(self.pref);
        for v in &mut z[self.nv..self.nv + self.nc] {
            *v = (p0 - self.pref) / self.ps;
        }
        z
    }

    #[must_use]
    pub fn fields(&self, z: &[f64]) -> FluidFaces {
        let faces: [Vec<f64>; 3] = std::array::from_fn(|a| {
            self.maps[a].iter().map(|id| usize::try_from(*id).map_or(0.0, |i| self.us * z[i])).collect()
        });
        let pressure: Vec<f64> = z[self.nv..self.nv + self.nc].iter().map(|v| self.ps * v).collect();
        let temperature: Vec<f64> = z[self.nv + self.nc..].iter().map(|v| self.t0 + self.ts * v).collect();
        let velocity = ndindex(self.grid)
            .map(|cell| {
                std::array::from_fn(|a| {
                    let mut hi = cell;
                    hi[a] += 1;
                    0.5 * (faces[a][flat(self.map_shapes[a], cell)] + faces[a][flat(self.map_shapes[a], hi)])
                })
            })
            .collect();
        FluidFaces { faces, pressure, temperature, velocity }
    }

    pub fn check(&self, n: usize, z: &[f64], _old: &[f64], x: &[f64]) -> CaeResult<()> {
        let t: Vec<f64> = z[self.nv + self.nc..].iter().map(|v| self.t0 + self.ts * v).collect();
        let fraction: Vec<f64> = x[..self.nc].iter().map(|v| 1.0 - v).collect();
        let active: Vec<bool> = fraction.iter().map(|f| *f > 0.0).collect();
        let (lower, upper) = (self.card.lower, self.card.upper);
        let phase_aware = self.inactive_phase_numerical_material.is_some();
        let invalid_at = |v: f64| !v.is_finite() || v <= 0.0 || v < lower || v > upper;
        let report_only = self.p["applicability_policy"].as_str() == Some("report_only");
        let invalid = if report_only {
            z.iter().any(|v| !v.is_finite()) || t.iter().any(|v| !v.is_finite() || *v <= 0.0)
        } else if phase_aware {
            t.iter().any(|v| !v.is_finite() || *v <= 0.0)
                || t.iter().zip(&active).any(|(v, a)| *a && invalid_at(*v))
        } else {
            t.iter().any(|v| invalid_at(*v))
        };
        if invalid {
            let argmin = argext(&t, true);
            let argmax = argext(&t, false);
            let tmin = t[argmin];
            let tmax = t[argmax];
            let active_t: Vec<f64> = t.iter().zip(&active).filter(|(_, a)| **a).map(|(v, _)| *v).collect();
            let active_count = active_t.len();
            let opt = |v: Option<f64>| v.map_or_else(|| "none".to_string(), repr_float);
            let amin = (active_count > 0).then(|| active_t.iter().copied().fold(f64::INFINITY, f64::min));
            let amax = (active_count > 0).then(|| active_t.iter().copied().fold(f64::NEG_INFINITY, f64::max));
            let grid_ix = |i: usize| {
                py_tuple([
                    i / (self.grid[1] * self.grid[2]),
                    (i / self.grid[2]) % self.grid[1],
                    i % self.grid[2],
                ])
            };
            return Err(CaeError::convergence(format!(
                "fluid temperature outside authored material validity interval (evaluation domain); evaluation_lower_K={}; evaluation_upper_K={}; original_material_lower_K={}; original_material_upper_K={}; history_step={n}; all_cell_temperature_min_K={}; all_cell_temperature_min_flat_index={argmin}; all_cell_temperature_min_grid_index={}; all_cell_temperature_min_physical_fluid_fraction={}; all_cell_temperature_max_K={}; all_cell_temperature_max_flat_index={argmax}; all_cell_temperature_max_grid_index={}; all_cell_temperature_max_physical_fluid_fraction={}; active_physical_fluid_cell_count={active_count}; active_physical_fluid_temperature_min_K={}; active_physical_fluid_temperature_max_K={}",
                repr_float(lower),
                repr_float(upper),
                repr(&self.card.raw["T_min_K"]),
                repr(&self.card.raw["T_max_K"]),
                fmt_g(tmin, 17),
                grid_ix(argmin),
                fmt_g(fraction[argmin], 17),
                fmt_g(tmax, 17),
                grid_ix(argmax),
                fmt_g(fraction[argmax], 17),
                opt(amin),
                opt(amax)
            )));
        }
        let p: Vec<f64> = z[self.nv..self.nv + self.nc].iter().map(|v| self.pref + self.ps * v).collect();
        if p.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::convergence("nonfinite absolute fluid pressure in numerical fluid state"));
        }
        let scope_min = p
            .iter()
            .zip(&active)
            .filter(|(_, a)| !phase_aware || **a)
            .map(|(v, _)| *v)
            .fold(f64::INFINITY, f64::min);
        if scope_min.is_finite() && scope_min <= 0.0 {
            return Err(CaeError::convergence(
                "nonpositive absolute fluid pressure in physical-fluid validity scope",
            ));
        }
        Ok(())
    }

    fn spacing_m(&self, x: &[f64]) -> [f64; 3] {
        std::array::from_fn(|a| x[self.nc + a] * 1e-3)
    }

    #[must_use]
    pub fn boundary_energy(&self, n: usize, z: &[f64], x: &[f64]) -> Vec<(String, f64)> {
        let f = self.fields(z);
        let h = self.spacing_m(x);
        let area_of = |axis: usize| h[0] * h[1] * h[2] / h[axis];
        let mut out = Vec::new();
        for r in &self.thermal_boundary_records {
            let lo = r.side == "lo";
            let sign = if lo { -1.0 } else { 1.0 };
            let area = area_of(r.axis);
            let pressure = r.boundary["momentum"].as_str() == Some("pressure");
            let temperature = r.boundary["thermal"].as_str() == Some("temperature");
            let mut val = 0.0;
            for cell in &r.cells {
                let mut ix = *cell;
                if !lo {
                    ix[r.axis] += 1;
                }
                let un = sign * f.faces[r.axis][flat(self.map_shapes[r.axis], ix)];
                let tt = f.temperature[self.cell_index(*cell)];
                if pressure {
                    let hin = self
                        .law
                        .enthalpy(r.boundary["incoming_temperature_K"][n].as_f64().unwrap_or(f64::NAN));
                    let hh = self.law.enthalpy(tt);
                    val += self.rho * area * (un.max(0.0) * hh + un.min(0.0) * hin);
                }
                if temperature {
                    let tb = r.boundary["temperature_K"][n].as_f64().unwrap_or(f64::NAN);
                    val +=
                        2.0 * self.law.fraction(x[self.cell_index(*cell)]) * self.law.properties(tt).1 * area
                            / h[r.axis]
                            * (tt - tb);
                }
            }
            out.push((format!("{}_{}", r.axis, r.side), val));
        }
        out
    }

    fn opening_pressure(&self, side: &str, n: usize) -> f64 {
        self.boundary(self.flow_axis, side)
            .and_then(|b| b.raw["pressure_absolute_Pa"][n].as_f64())
            .unwrap_or(f64::NAN)
    }

    #[must_use]
    pub fn metrics(&self, n: usize, z: &[f64], x: &[f64]) -> FluidMetrics {
        let f = self.fields(z);
        let h = self.spacing_m(x);
        let axis = self.flow_axis;
        let area = h[0] * h[1] * h[2] / h[axis];
        let shape = self.map_shapes[axis];
        let (mut qlo, mut qhi) = (0.0, 0.0);
        for ix in ndindex(shape) {
            if ix[axis] == 0 {
                qlo += f.faces[axis][flat(shape, ix)];
            }
            if ix[axis] == shape[axis] - 1 {
                qhi += f.faces[axis][flat(shape, ix)];
            }
        }
        qlo *= area;
        qhi *= area;
        let dp = self.opening_pressure("lo", n) - self.opening_pressure("hi", n);
        let volume = h[0] * h[1] * h[2];
        let energy: f64 = f
            .temperature
            .iter()
            .zip(&x[..self.nc])
            .map(|(t, th)| self.law.fraction(*th) * self.rho * self.law.enthalpy(*t))
            .sum::<f64>()
            * volume;
        let mut body_power = 0.0;
        if let Some((g, beta, tref)) = self.params.body {
            let mut total = 0.0;
            for (t, u) in f.temperature.iter().zip(&f.velocity) {
                let factor = self.rho * (1.0 - beta * (t - tref));
                for a in 0..3 {
                    total += factor * g[a] * u[a];
                }
            }
            body_power = total * volume;
        }
        FluidMetrics {
            flow_in: qlo,
            flow_out: qhi,
            hydraulic_power: dp * 0.5 * (qlo + qhi),
            enthalpy: energy,
            body_force_power: body_power,
            temperature_peak: f.temperature.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            pressure_absolute_min: self.pref + f.pressure.iter().copied().fold(f64::INFINITY, f64::min),
        }
    }

    #[must_use]
    pub fn wall_trace(&self, z: &[f64], x: &[f64], axis: usize, side: &str) -> Vec<[f64; 3]> {
        let f = self.fields(z);
        let h = self.spacing_m(x);
        let lo = side == "lo";
        let sign = if lo { 1.0 } else { -1.0 };
        let mut normal = [0.0; 3];
        normal[axis] = sign;
        let mut out = Vec::new();
        for cell in self.face_cells(axis, side) {
            let mu = self.law.properties(f.temperature[self.cell_index(cell)]).0;
            let mut nf = cell;
            if lo {
                nf[axis] += 1;
            }
            let dn = sign * f.faces[axis][flat(self.map_shapes[axis], nf)] / h[axis];
            let mut viscous = normal.map(|v| 2.0 * mu * dn * v);
            for b in 0..3 {
                if b == axis {
                    continue;
                }
                let mut tr = 0.0;
                for delta in [0usize, 1] {
                    let mut edge = cell;
                    edge[b] += delta;
                    let pos = i64::try_from(cell[b]).unwrap_or(0) + if delta == 0 { -1 } else { 1 };
                    let adj = if pos < 0 || pos >= i64::try_from(self.grid[b]).unwrap_or(0) {
                        let bd = self.boundary(b, if delta == 0 { "lo" } else { "hi" });
                        if bd.is_some_and(|bd| bd.pressure) {
                            continue;
                        }
                        cell
                    } else {
                        let mut a2 = cell;
                        a2[b] = usize::try_from(pos).unwrap_or(0);
                        a2
                    };
                    let mue = 0.5 * (mu + self.law.properties(f.temperature[self.cell_index(adj)]).0);
                    tr += mue * f.faces[b][flat(self.map_shapes[b], edge)] / h[axis];
                }
                viscous[b] = tr;
            }
            let pabs = self.pref + f.pressure[self.cell_index(cell)];
            out.push(std::array::from_fn(|a| -pabs * normal[a] + viscous[a]));
        }
        out
    }

    pub(crate) fn cell_viscosity(&self, mu: f64, theta: f64, f: &FluidFaces, cell: [usize; 3], h: [f64; 3]) -> f64 {
        let Some(ml) = &self.params.turbulence else { return mu };
        let mut rate = 0.0;
        for a in 0..3 {
            let mut hi = cell;
            hi[a] += 1;
            let shape = self.map_shapes[a];
            let d = (f.faces[a][flat(shape, hi)] - f.faces[a][flat(shape, cell)]) / h[a];
            rate += 2.0 * d * d;
        }
        mu + ml.eddy_viscosity(self.params.rho, 1.0 - theta, rate)
    }

    #[must_use]
    pub fn dissipation_w(&self, z: &[f64], x: &[f64]) -> f64 {
        let f = self.fields(z);
        let h = self.spacing_m(x);
        let volume = h[0] * h[1] * h[2];
        let mu: Vec<f64> = f.temperature.iter().map(|t| self.law.properties(*t).0).collect();
        let mut diss = 0.0;
        for a in 0..3 {
            let shape = self.map_shapes[a];
            let (mut normal, mut drag) = (0.0, 0.0);
            for cell in ndindex(self.grid) {
                let mut hi = cell;
                hi[a] += 1;
                let (l, u) = (f.faces[a][flat(shape, cell)], f.faces[a][flat(shape, hi)]);
                let i = self.cell_index(cell);
                normal +=
                    2.0 * self.cell_viscosity(mu[i], x[i], &f, cell, h) * volume * ((u - l) / h[a]).powi(2);
                drag += self.params.alpha(x[i]) * volume * 0.5 * (l * l + u * u);
            }
            diss += normal;
            diss += drag;
        }
        for r in &self.shear_records {
            let vel: Vec<f64> =
                r.iv.iter().map(|id| usize::try_from(*id).map_or(0.0, |i| self.us * z[i])).collect();
            let shear = r.factors[0] * (vel[1] - vel[0]) / h[r.b] + r.factors[1] * (vel[3] - vel[2]) / h[r.a];
            let mut mue: f64 = r.w.iter().zip(&r.ci).map(|(w, c)| w * mu[*c]).sum();
            if let Some(ml) = &self.params.turbulence {
                let phi: f64 = r.w.iter().zip(&r.ci).map(|(w, c)| w * (1.0 - x[*c])).sum();
                mue += ml.eddy_viscosity(self.params.rho, phi, shear * shear);
            }
            diss += mue * volume * r.vf * shear * shear;
        }
        diss
    }


    #[allow(clippy::too_many_lines)]
    pub fn validity(&self, z: &[f64], x: &[f64]) -> CaeResult<Value> {
        let f = self.fields(z);
        let h = self.spacing_m(x);
        let axes: Vec<usize> = (0..3).filter(|a| *a != self.flow_axis).collect();
        let lengths: [f64; 3] = std::array::from_fn(|a| self.grid[a] as f64 * h[a]);
        let d = 2.0 * lengths[axes[0]] * lengths[axes[1]] / (lengths[axes[0]] + lengths[axes[1]]);
        let speed: Vec<f64> = f.velocity.iter().map(norm3).collect();
        let props: Vec<(f64, f64, f64)> = f.temperature.iter().map(|t| self.law.properties(*t)).collect();
        let hydraulic_re: Vec<f64> = speed.iter().zip(&props).map(|(s, p)| self.rho * s * d / p.0).collect();
        let cell_length = h.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let cell_re: Vec<f64> =
            speed.iter().zip(&props).map(|(s, p)| self.rho * s * cell_length / p.0).collect();
        let cell_pe: Vec<f64> =
            speed.iter().zip(&props).map(|(s, p)| self.rho * p.2 * s * cell_length / p.1).collect();
        let maxf = |v: &[f64]| v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let (all_re, all_local_re, all_pe) = (maxf(&hydraulic_re), maxf(&cell_re), maxf(&cell_pe));
        let volume = h[0] * h[1] * h[2];
        let mut mass = vec![0.0; self.nc];
        for a in 0..3 {
            let shape = self.map_shapes[a];
            for cell in ndindex(self.grid) {
                let mut hi = cell;
                hi[a] += 1;
                mass[self.cell_index(cell)] +=
                    (f.faces[a][flat(shape, hi)] - f.faces[a][flat(shape, cell)]) * volume / h[a];
            }
        }
        let max_mass = mass.iter().map(|m| m.abs()).fold(f64::NEG_INFINITY, f64::max);
        let face_areas = h.map(|ha| volume / ha);
        let mass_scale = self.us * face_areas.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let mass_tolerance = MASS_BALANCE_RELATIVE_TOLERANCE * mass_scale;
        let absolute: Vec<f64> = f.pressure.iter().map(|p| self.pref + p).collect();
        let physical: Vec<f64> = x[..self.nc].iter().map(|v| 1.0 - v).collect();
        let active: Vec<bool> = physical.iter().map(|f| *f > 0.0).collect();
        let phase_aware = self.inactive_phase_numerical_material.is_some();
        let scope: Vec<bool> = if phase_aware { active.clone() } else { vec![true; self.nc] };
        let has_admission = scope.iter().any(|s| *s);
        let scoped_max = |v: &[f64], s: &[bool]| -> f64 {
            let vals: Vec<f64> = v.iter().zip(s).filter(|(_, k)| **k).map(|(x, _)| *x).collect();
            if vals.is_empty() { 0.0 } else { maxf(&vals) }
        };
        let scoped_min = |v: &[f64], s: &[bool]| -> Option<f64> {
            let vals: Vec<f64> = v.iter().zip(s).filter(|(_, k)| **k).map(|(x, _)| *x).collect();
            (!vals.is_empty()).then(|| vals.iter().copied().fold(f64::INFINITY, f64::min))
        };
        let re = scoped_max(&hydraulic_re, &scope);
        let local_re = scoped_max(&cell_re, &scope);
        let pe = scoped_max(&cell_pe, &scope);
        let active_re = scoped_max(&hydraulic_re, &active);
        let active_local_re = scoped_max(&cell_re, &active);
        let active_pe = scoped_max(&cell_pe, &active);
        let active_pmin = scoped_min(&absolute, &active);
        let admission_pmin = scoped_min(&absolute, &scope);
        let all_pmin = absolute.iter().copied().fold(f64::INFINITY, f64::min);
        let all_finite = |v: &[f64]| v.iter().all(|x| x.is_finite());
        let finite = f.faces.iter().all(|v| all_finite(v))
            && all_finite(&f.temperature)
            && all_finite(&f.pressure)
            && f.velocity.iter().all(|u| u.iter().all(|x| x.is_finite()))
            && props.iter().all(|p| p.0.is_finite() && p.1.is_finite() && p.2.is_finite())
            && all_finite(&h)
            && all_finite(&mass)
            && all_finite(&hydraulic_re)
            && all_finite(&cell_re)
            && all_finite(&cell_pe)
            && all_finite(&physical)
            && d.is_finite()
            && all_pmin.is_finite();
        let (tmin, tmax) = (self.card.t_min, self.card.t_max);
        let tlo = f.temperature.iter().copied().fold(f64::INFINITY, f64::min);
        let thi = f.temperature.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let all_temperature_valid = finite && tlo >= tmin && thi <= tmax;
        let active_temperature_valid = finite
            && f.temperature.iter().zip(&active).filter(|(_, a)| **a).all(|(t, _)| *t >= tmin && *t <= tmax);
        let temperature_valid = if phase_aware { active_temperature_valid } else { all_temperature_valid };
        let support: Vec<bool> = if phase_aware { active.clone() } else { vec![true; self.nc] };
        let domain = interval_status(
            &f.temperature,
            (tmin, tmax),
            (self.card.lower, self.card.upper),
            Some(&support),
        )?;
        let evaluation_ok = domain["evaluation_ok"].as_bool() == Some(true);
        let temperature_evaluable = finite && evaluation_ok && f.temperature.iter().all(|t| *t > 0.0);
        let inactive_outside =
            f.temperature.iter().zip(&active).filter(|(t, a)| !**a && (**t < tmin || **t > tmax)).count();
        let pressure_valid = finite && (!has_admission || admission_pmin.is_some_and(|p| p > 0.0));
        let all_pressure_positive = finite && all_pmin > 0.0;
        let mass_valid = finite && max_mass <= mass_tolerance;
        let n = &self.p["numerics"];
        let max_pe = n["max_cell_peclet"].as_f64().unwrap_or(f64::NAN);
        let cell_pe_valid = finite && pe <= max_pe;
        let local_limit = n.get("max_cell_reynolds").and_then(Value::as_f64);
        let local_re_valid = local_limit.map(|l| finite && local_re <= l);
        let legacy_limit = n["max_reynolds"].as_f64().unwrap_or(f64::NAN);
        let legacy_passed = finite && re <= legacy_limit;
        let reynolds_screen = local_re_valid.unwrap_or(legacy_passed);
        let policy_authored = n.get("regime_screen_policy").is_some();
        let regime_policy = n.get("regime_screen_policy").and_then(Value::as_str).unwrap_or("reject");
        let regime_passed = cell_pe_valid && reynolds_screen;
        let admissible = finite
            && temperature_evaluable
            && pressure_valid
            && mass_valid
            && (if regime_policy == "reject" { regime_passed } else { true });
        let scope_name =
            if phase_aware { "every_positive_physical_fluid_fraction_cell" } else { "all_cells_legacy" };
        let legacy_used = local_limit.is_none() && regime_policy == "reject";
        let policy = self.inactive_phase_numerical_material.as_ref();
        let mut out = Map::new();
        let mut put = |k: &str, v: Value| {
            out.insert(k.to_string(), v);
        };
        put("maximum_reynolds", json!(re));
        put("maximum_hydraulic_reynolds", json!(re));
        put("maximum_global_hydraulic_reynolds", json!(re));
        put("maximum_local_cell_reynolds", json!(local_re));
        put("maximum_cell_peclet", json!(pe));
        put("physical_fluid_maximum_hydraulic_reynolds", json!(active_re));
        put("physical_fluid_maximum_local_cell_reynolds", json!(active_local_re));
        put("physical_fluid_maximum_cell_peclet", json!(active_pe));
        put("physical_fluid_pressure_absolute_min_Pa", json!(active_pmin));
        put("all_cell_numerical_maximum_hydraulic_reynolds", json!(all_re));
        put("all_cell_numerical_maximum_local_cell_reynolds", json!(all_local_re));
        put("all_cell_numerical_maximum_cell_peclet", json!(all_pe));
        put("all_cell_numerical_pressure_absolute_min_Pa", json!(all_pmin));
        put("max_cell_mass_balance_m3_s", json!(max_mass));
        put("hydraulic_diameter_m", json!(d));
        put("cell_characteristic_length_m", json!(cell_length));
        put("fluid_regime_admission_cell_scope", json!(scope_name));
        put(
            "hydraulic_reynolds_definition",
            json!(
                "maximum_over_fluid_regime_admission_cell_scope(rho*cell_center_speed*full_cross_section_hydraulic_diameter/mu(T))"
            ),
        );
        put(
            "local_cell_reynolds_definition",
            json!(
                "maximum_over_fluid_regime_admission_cell_scope(rho*cell_center_speed*maximum_cell_edge_length/mu(T))"
            ),
        );
        put(
            "cell_peclet_definition",
            json!(
                "maximum_over_fluid_regime_admission_cell_scope(rho*cp(T)*cell_center_speed*maximum_cell_edge_length/k(T))"
            ),
        );
        put(
            "all_cell_numerical_regime_diagnostics_definition",
            json!(
                "same pointwise formulas over every numerical fluid cell, including exact-zero physical-fluid ghost cells; diagnostic only in phase-aware mode"
            ),
        );
        put("mass_balance_scale_m3_s", json!(mass_scale));
        put("mass_balance_tolerance_m3_s", json!(mass_tolerance));
        put("mass_balance_relative_tolerance", json!(MASS_BALANCE_RELATIVE_TOLERANCE));
        put("legacy_reference_maximum_reynolds", json!(legacy_limit));
        put("legacy_reference_passed", json!(legacy_passed));
        put("legacy_reference_used_for_numerical_admission", json!(legacy_used));
        put(
            "legacy_reference_role",
            json!(if legacy_used {
                "backward_compatible_global_hydraulic_admission_limit_not_an_operating_qualification_limit"
            } else {
                "inherited_global_hydraulic_reference_not_a_numerical_admission_or_transition_qualification_limit"
            }),
        );
        put("maximum_local_cell_reynolds_limit", json!(local_limit));
        put(
            "cell_reynolds_limit_provenance",
            n.get("cell_reynolds_limit_provenance").cloned().unwrap_or(Value::Null),
        );
        put("local_cell_reynolds_screen_applied", json!(local_limit.is_some()));
        put("local_cell_reynolds_screen_passed", json!(local_re_valid));
        put("maximum_cell_peclet_limit", json!(max_pe));
        put("cell_peclet_screen_passed", json!(cell_pe_valid));
        put("mass_conservation_screen_passed", json!(mass_valid));
        put("finite_state_screen_passed", json!(finite));
        put("temperature_material_interval_screen_passed", json!(temperature_valid));
        put("temperature_evaluation_domain_screen_passed", json!(temperature_evaluable));
        put("temperature_domain_status", domain);
        put(
            "material_evaluation_domain",
            self.card.raw.get("evaluation_domain").cloned().unwrap_or(Value::Null),
        );
        put("temperature_material_interval_screen_semantics", json!(scope_name));
        put("all_cell_temperature_material_interval_screen_passed", json!(all_temperature_valid));
        put("physical_fluid_temperature_material_interval_screen_passed", json!(active_temperature_valid));
        put("positive_physical_fluid_cell_count", json!(active.iter().filter(|a| **a).count()));
        put("exact_zero_physical_fluid_ghost_outside_interval_cell_count", json!(inactive_outside));
        put("applicability_policy", self.p.get("applicability_policy").cloned().unwrap_or(json!("enforce")));
        put("numerical_continuation_scope", json!(if self.p["applicability_policy"].as_str() == Some("report_only") { "all_positive_temperature_states_for_optimization_exploration" } else { "inactive_phase_only" }));
        put("physical_material_extrapolation_authorized", json!(self.p["applicability_policy"].as_str() == Some("report_only")));
        put("inactive_phase_numerical_material_enabled", json!(phase_aware));
        put("inactive_phase_numerical_material_schema", policy.map_or(Value::Null, |p| p["schema"].clone()));
        put(
            "inactive_phase_numerical_material_provenance",
            policy.map_or(Value::Null, |p| p["provenance"].clone()),
        );
        put("positive_absolute_pressure_screen_passed", json!(pressure_valid));
        put("positive_absolute_pressure_screen_semantics", json!(scope_name));
        put("all_cell_numerical_positive_absolute_pressure_passed", json!(all_pressure_positive));
        if policy_authored {
            put("regime_screen_policy", json!(regime_policy));
            put("regime_screen_policy_provenance", n["regime_screen_policy_provenance"].clone());
            put("regime_screens_passed", json!(regime_passed));
        }
        put("laminar_branch_screening_admissible", json!(admissible && regime_passed));
        put("numerical_discretization_admissible", json!(admissible));
        put("regime_valid", json!(admissible));
        put(
            "regime_valid_semantics",
            json!(
                "backward_compatible_alias_for_numerical_discretization_admissible_not_physical_regime_qualification"
            ),
        );
        put("transition_resolved", json!(false));
        put("operating_regime_qualified", json!(false));
        put("physical_qualification", json!(false));
        put("branch_contract", json!(BRANCH_CONTRACT));
        put(
            "branch_threshold_provenance",
            json!(
                "legacy cases retain their authored global Reynolds gate; explicitly opted-in cases use authored cell Peclet/cell Reynolds numerical screens and report global Reynolds without treating it as a validated transition criterion"
            ),
        );
        Ok(Value::Object(out))
    }

    #[must_use]
    pub fn report(&self) -> Value {
        let n = &self.p["numerics"];
        let local_limit = n.get("max_cell_reynolds").and_then(Value::as_f64);
        let policy = self.inactive_phase_numerical_material.as_ref();
        let regime_policy = n.get("regime_screen_policy").and_then(Value::as_str).unwrap_or("reject");
        let reject = regime_policy == "reject";
        let mut admission = vec![
            json!("finite_state"),
            json!(if policy.is_none() {
                "evaluation_temperature_interval_all_cells"
            } else {
                "evaluation_temperature_interval_every_positive_physical_fluid_fraction_cell"
            }),
            json!("positive_absolute_pressure"),
            json!("cell_mass_conservation"),
        ];
        if reject {
            admission.push(json!("authored_cell_peclet"));
            admission.push(json!(if local_limit.is_none() {
                "legacy_global_hydraulic_reynolds"
            } else {
                "authored_local_cell_reynolds"
            }));
        }
        let mut contract = Map::new();
        contract.insert("schema".into(), json!(BRANCH_CONTRACT));
        contract.insert("numerical_admission".into(), json!(admission));
        if n.get("regime_screen_policy").is_some() {
            contract.insert("regime_screen_policy".into(), n["regime_screen_policy"].clone());
            contract.insert(
                "regime_screen_policy_provenance".into(),
                n["regime_screen_policy_provenance"].clone(),
            );
        }
        let legacy_used = local_limit.is_none() && reject;
        for (k, v) in [
            (
                "pressure_reynolds_peclet_cell_scope",
                json!(if policy.is_none() {
                    "all_cells_legacy"
                } else {
                    "every_positive_physical_fluid_fraction_cell"
                }),
            ),
            (
                "all_cell_numerical_diagnostics",
                json!([
                    "finite_state",
                    "cell_mass_conservation",
                    "pressure_absolute_min",
                    "hydraulic_reynolds",
                    "local_cell_reynolds",
                    "cell_peclet"
                ]),
            ),
            (
                "all_cell_numerical_diagnostics_admission_role",
                json!(if policy.is_none() {
                    "legacy_admission_scope"
                } else {
                    "diagnostic_only_except_finite_state_and_cell_mass_conservation"
                }),
            ),
            ("legacy_reference_maximum_reynolds", n["max_reynolds"].clone()),
            ("legacy_reference_used_for_numerical_admission", json!(legacy_used)),
            (
                "legacy_reference_role",
                json!(if legacy_used {
                    "backward_compatible_global_hydraulic_admission_limit"
                } else {
                    "reported_global_hydraulic_reference_only_not_numerical_admission"
                }),
            ),
            ("maximum_local_cell_reynolds_limit", json!(local_limit)),
            (
                "cell_reynolds_limit_provenance",
                n.get("cell_reynolds_limit_provenance").cloned().unwrap_or(Value::Null),
            ),
            ("local_cell_reynolds_screen_authored", json!(local_limit.is_some())),
            ("maximum_cell_peclet_limit", json!(n["max_cell_peclet"].as_f64())),
            ("mass_balance_relative_tolerance", json!(MASS_BALANCE_RELATIVE_TOLERANCE)),
            ("regime_valid_semantics", json!("numerical_discretization_admissibility_alias")),
            ("transition_resolved", json!(false)),
            ("operating_regime_qualified", json!(false)),
            ("physical_qualification", json!(false)),
            (
                "scope",
                json!(
                    "component branch admission; coupled history owners retain energy and phase-connectivity guards"
                ),
            ),
        ] {
            contract.insert(k.into(), v);
        }
        if let Some(Value::Number(v)) = contract.get("legacy_reference_maximum_reynolds").cloned() {
            contract.insert("legacy_reference_maximum_reynolds".into(), json!(v.as_f64()));
        }
        let bounds = [self.card.lower, self.card.upper];
        json!({"state_unknowns": self.state_size, "velocity_unknowns": self.nv, "pressure_cells": self.nc,
            "energy_cells": self.nc,
            "material_evaluation_domain": self.card.raw.get("evaluation_domain").cloned().unwrap_or(Value::Null),
            "material_authored_temperature_interval_K": [self.card.raw["T_min_K"].clone(), self.card.raw["T_max_K"].clone()],
            "material_evaluation_temperature_interval_K": bounds,
            "momentum": if self.p["momentum_advection"].as_bool() == Some(true) { "incompressible_Navier_Stokes" } else { "unsteady_Stokes" },
            "body_acceleration": self.p.get("body_acceleration").cloned().unwrap_or(Value::Null),
            "body_force_convention": "rho_ref*(1-beta*(T-T_ref))*acceleration; absolute pressure, not hydrostatically reduced; constant density in continuity, inertia and enthalpy; body work is mechanical, not a direct heat source",
            "energy": "cell_finite_volume_enthalpy_upwind_conservative",
            "viscous_stress": "MAC_symmetric_gradient_variable_viscosity",
            "assembly": "local_AD_sparse_incidence", "global_dense_jacobian_allocated": false,
            "geometry": "fixed_Cartesian_subdomain", "limitations": KERNEL_LIMITATIONS,
            "inactive_phase_numerical_material": {
                "enabled": policy.is_some(),
                "schema": policy.map_or(Value::Null, |p| p["schema"].clone()),
                "method": policy.map_or(Value::Null, |p| p["method"].clone()),
                "scope": policy.map_or(Value::Null, |p| p["scope"].clone()),
                "provenance": policy.map_or(Value::Null, |p| p["provenance"].clone()),
                "physical_material_extrapolation_authorized": false,
                "physical_validity_gate": "declared evaluation interval required in every cell with physical fluid fraction > 0; original authored interval reported separately; exact-zero physical-fluid cells alone may exceed the evaluation interval under numerical continuation",
                "continuation_temperature_interval_K": bounds},
            "laminar_branch_contract": Value::Object(contract)})
    }


    pub fn local_values(
        &self,
        set: GroupSet,
        n: usize,
        z: &[f64],
        x: &[f64],
    ) -> CaeResult<Vec<(Vec<f64>, Vec<i64>)>> {
        let mut out = Vec::new();
        for g in self.selected(set) {
            out.push((g.local_values(n, z, x)?, g.rows().values.clone()));
        }
        Ok(out)
    }
}

fn norm3(u: &[f64; 3]) -> f64 {
    (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt()
}

fn argext(v: &[f64], minimum: bool) -> usize {

    let mut best = 0;
    for (i, x) in v.iter().enumerate() {
        if x.is_nan() {
            return i;
        }
        let b = v[best];
        if (minimum && *x < b) || (!minimum && *x > b) {
            best = i;
        }
    }
    best
}

impl MacFluid for FluidKernel {
    fn grid(&self) -> [usize; 3] {
        self.grid
    }
    fn nc(&self) -> usize {
        self.nc
    }
    fn nv(&self) -> usize {
        self.nv
    }
    fn cell(&self, cell: [usize; 3]) -> usize {
        self.cell_index(cell)
    }
    fn face(&self, axis: usize, face: [usize; 3]) -> i64 {
        self.map(axis, face)
    }
    fn us(&self) -> f64 {
        self.us
    }
    fn t0(&self) -> f64 {
        self.t0
    }
    fn ts(&self) -> f64 {
        self.ts
    }
    fn pref(&self) -> f64 {
        self.pref
    }
    fn ps(&self) -> f64 {
        self.ps
    }
    fn viscosity<S: Scalar>(&self, temperature: S) -> S {
        self.law.properties(temperature).0
    }
}
