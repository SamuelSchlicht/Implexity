// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, LegacySingleArrayProviderCapabilities, ProviderCapabilities,
    ProviderProblem, Sensitivity,
};
use implexity_core::coupling_graph::{CouplingDeclaration, validate_declaration};
use implexity_core::orchestration::AddInAdapter;
use implexity_core::packages::InstallContext;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_optim::design::{NamedArrays, design_identity};
use implexity_optim::provider_ops::{DesignOp, DesignOperations, DesignSensitivities, DesignSensitivity};
use implexity_physics_solid::solid_history::{HOST_RESPONSES, SolidHistoryFactory, SolidKernel};
use implexity_solve::coupled_history::{CoupledHistoryAssembly, HistoryBlock, HistoryInterface};
use implexity_solve::native_history::{HistorySolution, HistorySolveOptions};

use crate::conjugate_interface::{ConjugateInterface, InterfaceObservables};
use crate::incompressible_transport::{FluidKernel, GroupSet, flat, ndindex};
use crate::unified_history::kernel::{FluidBlock, SolidBlock};

pub const NAME: &str = "native_conjugate_history";
pub const COORDS: [&str; 3] = ["model:control", "model:parameters", "model:spatial_fields"];
pub const RESPONSES: [&str; 9] = [
    "conjugate_solid_plastic_strain",
    "conjugate_solid_creep_strain",
    "conjugate_inelastic_heat_J",
    "conjugate_solid_temperature_peak_K",
    "conjugate_solid_mass_kg",
    "conjugate_elastic_energy_J",
    "conjugate_fluid_temperature_peak_K",
    "conjugate_hydraulic_power_W",
    "conjugate_mass_flow_kg_s",
];
pub const UNITS: [&str; 9] = ["1", "1", "J", "K", "kg", "J", "K", "W", "kg/s"];
pub const MAX_INTERFACE_DISPLACEMENT_OVER_CELL: f64 = 0.05;
pub const KINDS: [(&str, &str); 4] = [
    ("solid", "solid_history_field"),
    ("fluid", "fluid_history_field"),
    ("thermal_contact", "thermal_interface_residual"),
    ("fluid_traction", "fluid_structure_interface_load"),
];
pub const LIMITATIONS: [&str; 9] = [
    "Aligned Cartesian solid/fluid regions with a fixed partition; no topology transfer between regions.",
    "Without explicit preload_history initialization the prescribed seed is not generically in equilibrium with the initial interface load. A resolved preload initializes service, not a fluid/thermal steady state.",
    "Quasistatic structural equilibrium omits inertia and resonance; the incompressible fluid cannot resolve compressible pressure waves.",
    "Small-strain inelastic solid and fixed fluid geometry; wall-motion/ALE feedback is not included.",
    "Resolved laminar incompressible Newtonian fluid; no turbulence, boiling, CHF or DNBR.",
    "No irradiation, erosion, fracture or calibrated cyclic lifetime model is supplied by this provider.",
    "User material coefficients, load histories, validity bounds and normalization required.",
    "Fluid obstruction design is Brinkman regularisation, not a mechanically solved solid phase.",
    "No global dense state Jacobian; sparse-direct monolithic solve, not distributed HPC.",
];

fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}

fn finite(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite()),
        _ => None,
    }
}


pub fn selected(name: &str, kind: &str) -> CaeResult<Arc<dyn AddInAdapter>> {
    let row = implexity_core::registries::global().addins.get(name)?;
    match row.adapter.clone() {
        Some(a) if a.component_kind().as_deref() == Some(kind) => Ok(a),
        _ => contract(format!("{name}: inactive or incompatible {kind}")),
    }
}


#[allow(clippy::too_many_lines)]
pub fn normalise(problem: &Value) -> CaeResult<Value> {
    let required = ["components", "fluid", "interface", "name", "numerics", "solid"];
    let ok = problem.as_object().is_some_and(|m| {
        required.iter().all(|k| m.contains_key(*k))
            && m.keys().all(|k| required.contains(&k.as_str()) || k == "initialization")
    });
    if !ok {
        return contract(
            "conjugate history requires name, components, solid, fluid, interface and numerics, with optional initialization",
        );
    }
    let mut p = problem.clone();
    let components = p["components"].as_object().cloned().unwrap_or_default();
    let names: std::collections::BTreeSet<&str> = components.keys().map(String::as_str).collect();
    if names != KINDS.iter().map(|(k, _)| *k).collect() {
        return contract("explicit solid/fluid/contact/traction components required");
    }
    for (key, kind) in KINDS {
        selected(components[key].as_str().unwrap_or_default(), kind)?;
    }
    crate::unified_history::kernel::solid_factory(components["solid"].as_str().unwrap_or_default())?;
    p["solid"] = SolidHistoryFactory::validate(&p["solid"])?;
    p["fluid"] = crate::incompressible_transport::selected_fluid_factory(
        components["fluid"].as_str().unwrap_or_default(),
    )?
    .validate(&p["fluid"])?;
    let times = |v: &Value| -> Vec<f64> {
        v["times_s"].as_array().into_iter().flatten().filter_map(Value::as_f64).collect()
    };
    if times(&p["solid"]) != times(&p["fluid"]) {
        return contract("coupled time histories must be identical");
    }
    if let Some(init) = p.get("initialization").cloned() {
        p["initialization"] =
            crate::conjugate_preload::normalise_initialization(&init, times(&p["solid"]).len())?;
    }
    let interface = p["interface"].clone();
    let keys = ["axis", "contact_resistance_m2K_W", "max_displacement_over_cell", "provenance", "solid_side"];
    if !interface
        .as_object()
        .is_some_and(|m| m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)))
    {
        return contract(
            "interface geometry, physical contact resistance, displacement validity bound and provenance required",
        );
    }
    let axis = interface["axis"].as_u64().filter(|a| *a < 3 && interface["axis"].is_u64());
    let side = interface["solid_side"].as_str().filter(|s| matches!(*s, "lo" | "hi"));
    let (Some(axis), Some(side)) = (axis, side) else {
        return contract("invalid interface face");
    };
    let axis = axis as usize;
    let r = finite(&interface["contact_resistance_m2K_W"]);
    let d = finite(&interface["max_displacement_over_cell"]);
    let provenance = implexity_core::pyobj::py_str(&interface["provenance"]);
    if r.is_none_or(|r| r < 0.0)
        || d.is_none_or(|d| !(d > 0.0 && d <= MAX_INTERFACE_DISPLACEMENT_OVER_CELL))
        || provenance.trim().is_empty()
    {
        return contract(format!(
            "invalid interface resistance/validity/provenance: contact resistance >= 0, 0 < max_displacement_over_cell <= {MAX_INTERFACE_DISPLACEMENT_OVER_CELL} (sharp fixed interface) and a provenance are required"
        ));
    }
    let fs = if side == "hi" { "lo" } else { "hi" };
    for a in 0..3 {
        if a != axis && p["solid"]["grid"][a] != p["fluid"]["grid"][a] {
            return contract("interface tangential grid mismatch");
        }
    }
    let interfaces: Vec<(u64, String)> = p["fluid"]["boundaries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["thermal"] == "interface")
        .map(|b| (b["axis"].as_u64().unwrap_or(9), b["side"].as_str().unwrap_or_default().to_string()))
        .collect();
    if interfaces != vec![(axis as u64, fs.to_string())] {
        return contract("exactly the coupled fluid face must be marked interface");
    }
    for family in ["heat_fluxes", "thermal_exchanges"] {
        if p["solid"][family].as_array().into_iter().flatten().any(|b| b["axis"] == axis && b["side"] == side)
        {
            return contract("solid interface cannot also carry a prescribed heat flux/exchange");
        }
    }
    if p["solid"]["tractions"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|b| b["axis"] == axis && b["side"] == side)
    {
        return contract("solid interface traction already supplied by solved fluid stress");
    }
    let n = &p["numerics"];
    let n_ok = n.as_object().is_some_and(|o| {
        o.len() == 2
            && o.contains_key("tolerance")
            && o.contains_key("max_iterations")
            && o.values().all(|v| finite(v).is_some_and(|x| x > 0.0))
    }) && n["max_iterations"].as_f64().is_some_and(|v| v.fract() == 0.0);
    if !n_ok {
        return contract("explicit coupled tolerance and integer Newton iteration budget required");
    }
    Ok(p)
}

pub struct ConjugateKernel {
    pub p: Value,
    pub s: Arc<SolidKernel>,
    pub f: Arc<FluidKernel>,
    pub axis: usize,
    pub grid: [usize; 3],
    pub nc: usize,
    pub nt: usize,
    pub sidx: Vec<usize>,
    pub fidx: Vec<usize>,
    pub sx: Vec<usize>,
    pub fx: Vec<usize>,
    pub soffset: [f64; 3],
    pub foffset: [f64; 3],
    pub assembly: Arc<CoupledHistoryAssembly>,
    pub interface: Arc<ConjugateInterface>,
    pub state_size: usize,
    pub solid_slice: std::ops::Range<usize>,
    pub fluid_slice: std::ops::Range<usize>,
    pub initialization: Value,
    pub service_start_index: usize,
    last: Mutex<Option<(Vec<u64>, Arc<HistorySolution>)>>,
}

impl std::fmt::Debug for ConjugateKernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConjugateKernel")
            .field("grid", &self.grid)
            .field("state_size", &self.state_size)
            .finish_non_exhaustive()
    }
}

fn max_weights(values: &[f64]) -> (f64, Vec<f64>) {
    let m = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let ties = values.iter().filter(|v| **v == m).count().max(1) as f64;
    (m, values.iter().map(|v| if *v == m { 1.0 / ties } else { 0.0 }).collect())
}

impl ConjugateKernel {

    #[allow(clippy::too_many_lines)]
    pub fn new(p: Value) -> CaeResult<Self> {
        let components = &p["components"];
        for (key, kind) in KINDS {
            selected(components[key].as_str().unwrap_or_default(), kind)?;
        }
        let s = SolidHistoryFactory::create(&p["solid"])?;
        let f = crate::incompressible_transport::selected_fluid_factory(
            components["fluid"].as_str().unwrap_or_default(),
        )?
        .create(&p["fluid"])?;
        let axis = p["interface"]["axis"].as_u64().unwrap_or(0) as usize;
        let mut grid = s.grid;
        grid[axis] += f.grid[axis];
        let nc: usize = grid.iter().product();
        let solid_hi = p["interface"]["solid_side"] == "hi";
        let (mut sidx, mut fidx) = (Vec::new(), Vec::new());
        let (mut soffset, mut foffset) = ([0.0; 3], [0.0; 3]);
        if solid_hi {
            foffset[axis] = s.grid[axis] as f64;
        } else {
            soffset[axis] = f.grid[axis] as f64;
        }
        for cell in ndindex(grid) {
            let in_solid = if solid_hi { cell[axis] < s.grid[axis] } else { cell[axis] >= f.grid[axis] };
            if in_solid {
                sidx.push(flat(grid, cell));
            } else {
                fidx.push(flat(grid, cell));
            }
        }
        let sx: Vec<usize> =
            sidx.iter().copied().chain(nc..nc + 3).chain(sidx.iter().map(|i| nc + 3 + i)).collect();
        let fx: Vec<usize> = fidx.iter().copied().chain(nc..nc + 3).collect();
        let numerics = &p["numerics"];
        let tolerance = numerics["tolerance"].as_f64().unwrap_or(f64::NAN);
        let max_iterations = numerics["max_iterations"].as_f64().map_or(0, |v| v as usize);
        let blocks = vec![
            HistoryBlock {
                name: "solid".into(),
                initial: s.initial_state(),
                design_indices: sx.clone(),
                callbacks: Arc::new(SolidBlock(Arc::clone(&s))),
                field: None,
            },
            HistoryBlock {
                name: "fluid".into(),
                initial: f.initial_state(),
                design_indices: fx.clone(),
                callbacks: Arc::new(FluidBlock { f: Arc::clone(&f), set: GroupSet::All }),
                field: None,
            },
        ];
        let assembly =
            Arc::new(CoupledHistoryAssembly::new(blocks, 2 * nc + 3, tolerance, max_iterations, None)?);
        let solid_slice = assembly.slice("solid").map(|(a, b)| a..b).unwrap_or(0..0);
        let fluid_slice = assembly.slice("fluid").map(|(a, b)| a..b).unwrap_or(0..0);
        let interface = Arc::new(ConjugateInterface::new(
            assembly.state_size(),
            assembly.design_size(),
            solid_slice.start,
            fluid_slice.start,
            &s,
            &f,
            &sx,
            &fx,
            &p["interface"],
        )?);
        assembly.add_interface(Arc::clone(&interface) as Arc<dyn HistoryInterface>)?;
        let initialization = crate::conjugate_preload::initialization_description(&p);
        let service_start_index = initialization["service_start_index"].as_u64().unwrap_or(0) as usize;
        Ok(Self {
            state_size: assembly.state_size(),
            nt: s.nt,
            p,
            s,
            f,
            axis,
            grid,
            nc,
            sidx,
            fidx,
            sx,
            fx,
            soffset,
            foffset,
            assembly,
            interface,
            solid_slice,
            fluid_slice,
            initialization,
            service_start_index,
            last: Mutex::new(None),
        })
    }

    #[must_use]
    pub fn solid_design(&self, x: &[f64]) -> Vec<f64> {
        self.sx.iter().map(|i| x[*i]).collect()
    }

    #[must_use]
    pub fn fluid_design(&self, x: &[f64]) -> Vec<f64> {
        self.fx.iter().map(|i| x[*i]).collect()
    }

    fn solid_states(&self, states: &[Vec<f64>]) -> Vec<Vec<f64>> {
        states.iter().map(|z| z[self.solid_slice.clone()].to_vec()).collect()
    }


    pub fn solve(&self, x: &[f64]) -> CaeResult<Arc<HistorySolution>> {
        let ident: Vec<u64> = x.iter().map(|v| v.to_bits()).collect();
        if let Ok(last) = self.last.lock()
            && let Some((key, sol)) = last.as_ref()
            && *key == ident
        {
            return Ok(Arc::clone(sol));
        }
        let sol = self.assembly.solve(x, self.nt - 1, &HistorySolveOptions::default())?;
        let sx = self.solid_design(x);
        let fx = self.fluid_design(x);
        let solid = HistorySolution {
            states: self.solid_states(&sol.states),
            residual_norms: sol.residual_norms.clone(),
            newton_iterations: sol.newton_iterations.clone(),
            execution_context: None,
            convergence_reports: None,
        };
        self.s.validate_history(&solid, &sx)?;
        let hmin = (0..3).map(|a| x[self.nc + a] * 1e-3).fold(f64::INFINITY, f64::min);
        let bound = self.p["interface"]["max_displacement_over_cell"].as_f64().unwrap_or(f64::NAN);
        for n in 1..self.nt {
            let zf = &sol.states[n][self.fluid_slice.clone()];
            let of = &sol.states[n - 1][self.fluid_slice.clone()];
            self.f.check(n, zf, of, &fx)?;
            let reg = self.f.validity(zf, &fx)?;
            if reg["regime_valid"] != json!(true) {
                return Err(CaeError::convergence(crate::fluid_admission::fluid_admission_failure_message(
                    &crate::fluid_admission::admission_report(&reg),
                    n as i64,
                    "native_mac_cell_center_upwind",
                )));
            }
            let u = self.s.nodal_displacement(n, &solid.states[n]);
            let umax = self
                .interface
                .nodes
                .iter()
                .flatten()
                .map(|node| (u[*node][0].powi(2) + u[*node][1].powi(2) + u[*node][2].powi(2)).sqrt())
                .fold(f64::NEG_INFINITY, |a, v| if v > a || v.is_nan() { v } else { a });
            let ratio = umax / hmin;
            if !ratio.is_finite() || ratio > bound {
                return Err(CaeError::convergence(
                    "fixed fluid geometry approximation exceeds authored displacement/cell validity bound",
                ));
            }
        }
        let sol = Arc::new(sol);
        if let Ok(mut last) = self.last.lock() {
            *last = Some((ident, Arc::clone(&sol)));
        }
        Ok(sol)
    }


    pub fn certify_sensitivity(&self, sol: &HistorySolution, x: &[f64]) -> CaeResult<Value> {
        self.s.certify_sensitivity(&self.solid_states(&sol.states), &self.solid_design(x))
    }

    fn service_window(&self) -> bool {
        self.service_start_index > 0 && self.initialization["response_window"] == "service"
    }

    fn fluid_temperatures(&self, z: &[f64]) -> Vec<(f64, usize)> {
        let f = &self.f;
        (0..f.nc)
            .map(|c| {
                let row = self.fluid_slice.start + f.nv + f.nc + c;
                (f.t0 + f.ts * z[row], row)
            })
            .collect()
    }


    #[allow(clippy::too_many_lines, clippy::type_complexity)]
    pub fn responses(
        &self,
        states: &[Vec<f64>],
        x: &[f64],
        names: Option<&[String]>,
    ) -> CaeResult<(Vec<f64>, Option<(Vec<DenseMatrix>, DenseMatrix)>)> {
        let s = &self.s;
        let f = &self.f;
        let nt = self.nt;
        let sx = self.solid_design(x);
        let fx = self.fluid_design(x);
        let solid_states = self.solid_states(states);
        let grads = names.is_some();
        let service = self.service_window();
        let start = self.service_start_index;
        let solid = s.responses(&solid_states, &sx, grads)?;
        let preload = if service { Some(s.responses(&solid_states[..=start], &sx, grads)?) } else { None };
        let mut values: Vec<f64> = solid.values[..HOST_RESPONSES.len()].to_vec();

        let peak_steps: Vec<usize> = if service { (start..nt).collect() } else { Vec::new() };
        let mut peak_rows: Vec<(usize, Vec<f64>)> = Vec::new();
        if service {
            values[2] -= preload.as_ref().map_or(0.0, |p| p.values[2]);
            let temps: Vec<Vec<f64>> =
                peak_steps.iter().map(|n| s.nodal_temperature(*n, &solid_states[*n])).collect();
            let all: Vec<f64> = temps.iter().flatten().copied().collect();
            let (m, w) = max_weights(&all);
            values[3] = m;
            let nn = s.nn;
            peak_rows =
                peak_steps.iter().enumerate().map(|(i, n)| (*n, w[i * nn..(i + 1) * nn].to_vec())).collect();
        }

        let fluid_steps: Vec<usize> = if service { (start..nt).collect() } else { (1..nt).collect() };
        let per_step: Vec<(f64, Vec<(f64, usize)>, Vec<f64>)> = fluid_steps
            .iter()
            .map(|n| {
                let t = self.fluid_temperatures(&states[*n]);
                let (m, w) = max_weights(&t.iter().map(|(v, _)| *v).collect::<Vec<_>>());
                (m, t, w)
            })
            .collect();
        let (fluid_peak, step_weights) =
            max_weights(&per_step.iter().map(|(m, _, _)| *m).collect::<Vec<_>>());
        values.push(fluid_peak);
        let last = &states[nt - 1][self.fluid_slice.clone()];
        let metrics = f.metrics(nt - 1, last, &fx);
        values.push(metrics.hydraulic_power);
        values.push(f.rho * metrics.flow_out);
        let Some(names) = names else { return Ok((values, None)) };
        let idx: Vec<usize> = names
            .iter()
            .map(|r| {
                RESPONSES
                    .iter()
                    .position(|q| q == r)
                    .ok_or_else(|| CaeError::contract("invalid conjugate response set"))
            })
            .collect::<CaeResult<_>>()?;
        let m = names.len();
        let mut gu: Vec<DenseMatrix> = (0..nt).map(|_| DenseMatrix::zeros(self.state_size, m)).collect();
        let mut gx = DenseMatrix::zeros(x.len(), m);
        let ss = self.solid_slice.start;
        let axis = f.flow_axis;
        let h: [f64; 3] = std::array::from_fn(|a| x[self.nc + a] * 1e-3);
        let area = h[0] * h[1] * h[2] / h[axis];
        for (j, &i) in idx.iter().enumerate() {
            let add_solid = |gu: &mut Vec<DenseMatrix>,
                             gx: &mut DenseMatrix,
                             eval: &implexity_physics_solid::solid_history::ResponseEval,
                             col: usize,
                             sign: f64| {
                for (n, rows) in eval.gu.iter().enumerate() {
                    for (k, row) in rows.iter().enumerate() {
                        gu[n].data[(ss + k) * m + j] += sign * row[col];
                    }
                }
                for (k, row) in eval.gx.iter().enumerate() {
                    gx.data[self.sx[k] * m + j] += sign * row[col];
                }
            };
            match i {
                3 if service => {
                    for (n, w) in &peak_rows {
                        for (k, node) in s.free_t.iter().enumerate() {
                            gu[*n].data[(ss + k) * m + j] += w[*node] * s.model.ts;
                        }
                    }
                }
                2 if service => {
                    add_solid(&mut gu, &mut gx, &solid, 2, 1.0);
                    if let Some(p) = &preload {
                        add_solid(&mut gu, &mut gx, p, 2, -1.0);
                    }
                }
                i if i < HOST_RESPONSES.len() => add_solid(&mut gu, &mut gx, &solid, i, 1.0),
                6 => {
                    for ((n, (_, t, w)), sw) in fluid_steps.iter().zip(&per_step).zip(&step_weights) {
                        if *sw == 0.0 {
                            continue;
                        }
                        for ((_, row), cw) in t.iter().zip(w) {
                            gu[*n].data[row * m + j] += sw * cw * f.ts;
                        }
                    }
                }
                7 | 8 => {
                    let dp = {
                        let get = |side: &str| {
                            f.boundary(axis, side)
                                .and_then(|b| b.raw["pressure_absolute_Pa"][nt - 1].as_f64())
                                .unwrap_or(f64::NAN)
                        };
                        get("lo") - get("hi")
                    };
                    let (value, lo, hi) = if i == 7 {
                        (metrics.hydraulic_power, dp * 0.5 * area * f.us, dp * 0.5 * area * f.us)
                    } else {
                        (f.rho * metrics.flow_out, 0.0, f.rho * area * f.us)
                    };
                    let shape = f.map_shapes[axis];
                    for ix in ndindex(shape) {
                        let Ok(id) = usize::try_from(f.maps[axis][flat(shape, ix)]) else { continue };
                        let row = self.fluid_slice.start + id;
                        if ix[axis] == 0 {
                            gu[nt - 1].data[row * m + j] += lo;
                        }
                        if ix[axis] == shape[axis] - 1 {
                            gu[nt - 1].data[row * m + j] += hi;
                        }
                    }
                    for a in 0..3 {
                        if a != axis {
                            gx.data[(self.nc + a) * m + j] += value / x[self.nc + a];
                        }
                    }
                }
                _ => return contract("invalid conjugate response set"),
            }
        }
        Ok((values, Some((gu, gx))))
    }


    pub fn initial_state_observation(&self, x: &[f64], initial: &[f64]) -> CaeResult<Value> {
        let s = &self.s;
        let sx = self.solid_design(x);
        let zs = &initial[self.solid_slice.clone()];
        let fields = s.fields(0, zs, &sx);
        let stress: Vec<f64> = fields.iter().flat_map(|f| f.stress.to_vec()).collect();
        let trace = self.interface.observables(0, initial, x)?;
        let load: Vec<f64> = s.free_u.iter().map(|dof| trace.solid_nodal_load[dof / 3][dof % 3]).collect();
        let finite_all = stress.iter().chain(&load).all(|v| v.is_finite());
        let stress_free = stress.iter().all(|v| *v == 0.0);
        let loaded = load.iter().any(|v| *v != 0.0);
        let max_abs = |v: &[f64]| v.iter().fold(0.0_f64, |a, x| a.max(x.abs()));
        Ok(json!({
            "mode": "prescribed_stress_free_solid_and_initial_fluid_state",
            "equilibrium_solve_performed": false, "initial_equilibrium_certified": false,
            "available": finite_all,
            "maximum_initial_material_stress_Pa": if finite_all { json!(max_abs(&stress)) } else { Value::Null },
            "interface_load_on_free_displacements_l2_N": if finite_all { json!(load.iter().map(|v| v * v).sum::<f64>().sqrt()) } else { Value::Null },
            "nonzero_interface_load_with_zero_initial_stress": if finite_all { json!(stress_free && loaded) } else { Value::Null },
            "changes_initial_state": false, "changes_solve_admission": false,
            "note": "A prescribed initial state is not a coupled preload equilibrium. Use an explicitly authored preload_history to begin service at a solved coupled endpoint. The prescribed seed itself is not equilibrated.",
        }))
    }


    pub fn service_initial_observation(&self, x: &[f64], sol: &HistorySolution) -> CaeResult<Value> {
        let start = self.service_start_index;
        if start == 0 {
            return self.initial_state_observation(x, &sol.states[0]);
        }
        let s = &self.s;
        let z = &sol.states[start];
        let old = &sol.states[start - 1];
        let sx = self.solid_design(x);
        let fields = s.fields(start, &z[self.solid_slice.clone()], &sx);
        let stress: Vec<f64> = fields.iter().flat_map(|f| f.stress.to_vec()).collect();
        let trace = self.interface.observables(start, z, x)?;
        let load: Vec<f64> = s.free_u.iter().map(|dof| trace.solid_nodal_load[dof / 3][dof % 3]).collect();
        let residual = self.assembly.residual(start, z, old, x)?;
        let solid_residual = &residual[self.solid_slice.clone()];
        let scale = s.model.ss * s.model.ls * s.model.ls;
        let mechanical: Vec<f64> =
            solid_residual[s.displacement_row_slice()].iter().map(|v| v * scale).collect();
        let finite_all = residual.iter().chain(&stress).chain(&load).all(|v| v.is_finite());
        let max_abs = |v: &[f64]| v.iter().fold(0.0_f64, |a, x| a.max(x.abs()));
        let l2 = |v: &[f64]| v.iter().map(|x| x * x).sum::<f64>().sqrt();
        Ok(json!({
            "mode": "resolved_preload_history_endpoint",
            "source_state_index": start,
            "source_time_s": s.times[start], "service_time_s": 0.0,
            "equilibrium_solve_performed": true,
            "initial_equilibrium_certified": false,
            "available": finite_all,
            "maximum_initial_material_stress_Pa": max_abs(&stress),
            "interface_load_on_free_displacements_l2_N": l2(&load),
            "mechanical_free_residual_l2_N": l2(&mechanical),
            "mechanical_free_residual_max_N": max_abs(&mechanical),
            "coupled_residual_max_nondimensional": max_abs(&residual),
            "recorded_solver_residual_norm": sol.residual_norms[start - 1],
            "recorded_newton_iterations": sol.newton_iterations[start - 1],
            "authored_residual_tolerance": self.p["numerics"]["tolerance"],
            "nonzero_interface_load_with_zero_initial_stress": stress.iter().all(|v| *v == 0.0) && load.iter().any(|v| *v != 0.0),
            "changes_initial_state": true, "changes_solve_admission": false,
            "material_history_reset": false,
            "fluid_and_thermal_steady_equilibrium_claimed": false,
            "note": "Service begins at a solved quasistatic mechanical endpoint of the explicitly authored transient preload. The full internal, fluid and thermal state and its design dependence are retained. This is not an instantaneous or thermodynamic equilibrium certificate.",
        }))
    }


    #[allow(clippy::too_many_lines)]
    pub fn diagnostics(
        &self,
        x: &[f64],
        sol: &HistorySolution,
        design_state_id: &str,
    ) -> CaeResult<Map<String, Value>> {
        let s = &self.s;
        let f = &self.f;
        let m = &s.model;
        let sl = self.solid_slice.clone();
        let fl = self.fluid_slice.clone();
        let sx = self.solid_design(x);
        let fx = self.fluid_design(x);
        let h = [x[self.nc] * 1e-3, x[self.nc + 1] * 1e-3, x[self.nc + 2] * 1e-3];
        let volume = h[0] * h[1] * h[2] / 6.0;
        let states = &sol.states;
        let last_interface = self.interface.observables(self.nt - 1, &states[self.nt - 1], x)?;
        let mut history: Vec<Value> = Vec::new();
        for i in 1..self.nt {
            let ix = self.interface.observables(i, &states[i], x)?;
            let dt = s.times[i] - s.times[i - 1];
            let cur = f.metrics(i, &states[i][fl.clone()], &fx);
            let old = f.metrics(i - 1, &states[i - 1][fl.clone()], &fx);
            let external: f64 =
                f.boundary_energy(i, &states[i][fl.clone()], &fx).iter().map(|(_, v)| v).sum();
            let diss = f.dissipation_w(&states[i][fl.clone()], &fx);
            let fraction: f64 = self.fidx.iter().map(|c| f.law.fraction(x[*c])).sum();
            let source =
                f.p["volumetric_heat_W_m3"][i].as_f64().unwrap_or(0.0) * h[0] * h[1] * h[2] * fraction;
            let fluid_outward: f64 = ix.fluid_outward_power.iter().sum();
            let solid_outward: f64 = ix.solid_outward_power.iter().sum();
            let fluid_balance = (cur.enthalpy - old.enthalpy) / dt + external + fluid_outward - diss - source;
            let zs = &states[i][sl.clone()];
            let os = &states[i - 1][sl.clone()];
            let thermoelastic = solid_step_ledger(s, i, zs, os, &sx);
            let nodal_t = s.nodal_temperature(i, zs);
            let boundary_ledger = s.boundary.ledger(i, zs, os, &sx, &nodal_t);
            let nodal = implexity_physics_solid::solid_nodal_balance::solid_nodal_balance(
                s,
                i,
                zs,
                os,
                &sx,
                Some(&ix.solid_nodal_load),
                Some(&ix.solid_outward_power),
            )?;
            let temperature_reaction =
                nodal.summary["temperature_reaction_inward_W"].as_f64().unwrap_or(f64::NAN);
            let boundary_outward = boundary_ledger["solid_exchange_outward_W"].as_f64().unwrap_or(0.0);
            let reservoir_balance: f64 = boundary_ledger["finite_reservoirs"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(_, r)| r["balance_W"].as_f64().unwrap_or(0.0))
                .sum();
            let mut solid_balance: Option<f64> = None;
            let mut caloric = Map::new();
            let rho = &sx[..s.nc];
            let heat_sources = |s: &SolidKernel| -> f64 {
                let mut q = 0.0;
                for b in s.p["heat_fluxes"].as_array().into_iter().flatten() {
                    let axis = b["axis"].as_u64().unwrap_or(0) as usize;
                    let (_, w) = s.boundary_weights(axis, b["side"] == "hi", &h);
                    q += w.iter().sum::<f64>() * b["values"][i].as_f64().unwrap_or(0.0);
                }
                q
            };
            if thermoelastic.is_none() {
                let old_t = s.nodal_temperature(i - 1, os);
                let [a, b] = &m.materials;
                let mut storage = 0.0;
                for (e, tet) in s.mesh.tets.iter().enumerate() {
                    let o = s.mesh.owners[e];
                    let c = sx[s.nc + 3 + o];
                    let mut sum = 0.0;
                    for node in tet {
                        let (t, tp) = (nodal_t[*node], old_t[*node]);
                        let (ha, hb) = if m.numerical {
                            (
                                m.law.numerical_enthalpy_increment(a, t, tp),
                                m.law.numerical_enthalpy_increment(b, t, tp),
                            )
                        } else {
                            (m.law.enthalpy_increment(a, t, tp)?, m.law.enthalpy_increment(b, t, tp)?)
                        };
                        sum += (1.0 - c) * a.density() * ha + c * b.density() * hb;
                    }
                    storage += rho[o] * volume / 4.0 * sum;
                }
                let current = s.observe(i, zs, os, &sx);
                let previous = s.observe(i - 1, os, os, &sx);
                let heat = current.heat_increment.iter().sum::<f64>() * volume / dt;
                let weight =
                    |v: &[f64]| v.iter().zip(&s.mesh.owners).map(|(a, o)| a * rho[*o] * volume).sum::<f64>();
                let evolution = weight(&current.material_evolution_heat);
                let supply = weight(&current.material_external_energy);
                let delta: Vec<f64> = current
                    .material_stored_energy
                    .iter()
                    .zip(&previous.material_stored_energy)
                    .map(|(a, b)| a - b)
                    .collect();
                let material_rate = weight(&delta) / dt;
                let mut source_solid = heat_sources(s);
                source_solid += s.p["volumetric_heat_W_m3"][i].as_f64().unwrap_or(0.0)
                    * rho.iter().sum::<f64>()
                    * h[0]
                    * h[1]
                    * h[2];
                let balance = storage / dt + solid_outward + boundary_outward
                    - source_solid
                    - heat
                    - evolution
                    - temperature_reaction;
                caloric = json!({"solid_sensible_enthalpy_increment_J": storage, "solid_inelastic_heat_W": heat,
                    "solid_material_evolution_heat_W": evolution, "solid_material_external_source_W": supply,
                    "solid_material_stored_energy_rate_W": material_rate,
                    "solid_material_energy_balance_W": material_rate + evolution - supply,
                    "solid_caloric_thermal_balance_W": balance,
                    "total_caloric_thermal_balance_W": balance + fluid_balance,
                    "fields_and_finite_reservoirs_caloric_balance_W": balance + fluid_balance + reservoir_balance})
                .as_object()
                .cloned()
                .unwrap_or_default();
            } else if let Some(t) = &thermoelastic {
                let mut solid_heat = heat_sources(s);
                solid_heat += s.p["volumetric_heat_W_m3"][i].as_f64().unwrap_or(0.0)
                    * self.sidx.iter().map(|c| x[*c]).sum::<f64>()
                    * h[0]
                    * h[1]
                    * h[2];
                solid_balance = Some(
                    t["entropy_thermal_storage_J"].as_f64().unwrap_or(f64::NAN) / dt
                        + solid_outward
                        + boundary_outward
                        - solid_heat
                        - temperature_reaction,
                );
            }
            let start = self.service_start_index;
            let mut row = json!({
                "time_s": s.times[i], "history_index": i, "interval_duration_s": dt,
                "history_phase": if i <= start { "preload" } else { "service" },
                "service_time_s": s.times[i] - s.times[start],
                "interface_heat_solid_to_fluid_W": solid_outward,
                "interface_balance_W": ix.heat_balance, "fluid_energy_balance_W": fluid_balance,
                "solid_thermoelastic_step_ledger": thermoelastic.clone().unwrap_or_else(|| json!({})),
                "solid_entropy_thermal_balance_W": solid_balance,
                "fields_and_finite_reservoirs_entropy_thermal_balance_W": solid_balance.map(|b| b + fluid_balance + reservoir_balance),
                "solid_boundary_exchange_ledger": boundary_ledger,
                "solid_nodal_balance": nodal.summary,
                "solid_temperature_reaction_inward_W": temperature_reaction,
            });
            let obj = row.as_object_mut().ok_or_else(|| CaeError::contract("diagnostics row"))?;
            obj.extend(caloric);
            obj.insert("fluid_enthalpy_increment_J".into(), json!(cur.enthalpy - old.enthalpy));
            obj.insert("fluid_external_outward_heat_W".into(), json!(external));
            obj.insert("fluid_dissipation_W".into(), json!(diss));
            if let Some(mm) = cur.to_value().as_object() {
                obj.extend(mm.clone());
            }
            if let Some(v) = f.validity(&states[i][fl.clone()], &fx)?.as_object() {
                obj.extend(v.clone());
            }
            let step =
                crate::conjugate_energy_contract::observe_step(self, i, &states[i], &states[i - 1], x, &row)?;
            row["discrete_energy_step"] = step;
            history.push(row);
        }
        let solid_states = self.solid_states(states);
        let mut provenance: Vec<Value> =
            m.materials.iter().map(implexity_physics_solid::material::SolidMaterial::provenance).collect();
        provenance.push(f.card.raw["provenance"].clone());
        let report = self.assembly.report();
        let out = json!({
            "design_state_id": design_state_id, "components": self.p["components"],
            "coupled_assembly": report, "fluid_assembly": f.report(),
            "state_residual_norms": sol.residual_norms, "newton_iterations": sol.newton_iterations,
            "coupling_history": history, "interface_normal_solid_to_fluid": self.interface.normal,
            "energy_contract": implexity_physics_solid::solid_nodal_balance::conjugate_energy_contract(&history, self.service_start_index as i64),
            "discrete_energy_contract": crate::conjugate_energy_contract::history_contract(&history, self.service_start_index),
            "initial_state": self.service_initial_observation(x, sol)?,
            "history_seed_initial_state": self.initial_state_observation(x, &states[0])?,
            "initialization": self.initialization,
            "fatigue_observer": s.fatigue_diagnostics(&solid_states, &sx)?,
            "maximum_interface_temperature_residual_K": last_interface.maximum_temperature_interface_residual,
            "maximum_interface_force_balance_N": last_interface.force_balance.iter().fold(0.0_f64, |a, v| a.max(v.abs())),
            "response_units": RESPONSES.iter().zip(UNITS).map(|(r, u)| ((*r).to_string(), json!(u))).collect::<Map<_, _>>(),
            "physical_qualification": false, "regime_valid": true,
            "regime_valid_semantics": "numerical_discretization_admissibility_not_calibration_or_engineering_acceptance",
            "limitations": LIMITATIONS,
            "material_provenance": provenance,
            "state_layout": report["state_blocks"],
            "times_s": s.p["times_s"],
            "pressure_reference_absolute_Pa": f.pref,
            "pumping_metric": "hydraulic_dp_times_volumetric_flow_not_shaft_power",
            "fluid_material_design": "fixed authored fluid law; solid mixture coordinate has explicit zero derivatives in fluid region",
        });
        Ok(out.as_object().cloned().unwrap_or_default())
    }


    pub fn interface_history(&self, states: &[Vec<f64>], x: &[f64]) -> CaeResult<Vec<InterfaceObservables>> {
        (1..self.nt).map(|i| self.interface.observables(i, &states[i], x)).collect()
    }
}

#[must_use]
pub fn solid_step_ledger(s: &SolidKernel, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> Option<Value> {
    let m = &s.model;
    if !m.law.reversible_thermoelastic() {
        return None;
    }
    let cur = s.fields(n, z, x);
    let prev = s.fields(n - 1, old, x);
    let t = s.nodal_temperature(n, z);
    let tp = s.nodal_temperature(n - 1, old);
    let volume = x[s.nc] * x[s.nc + 1] * x[s.nc + 2] * 1e-9 / 6.0;
    let mut totals: BTreeMap<String, f64> = BTreeMap::new();
    for (e, tet) in s.mesh.tets.iter().enumerate() {
        let o = s.mesh.owners[e];
        let density = x[o];
        let stiffness = m.stiffness(density);
        let te: [f64; 4] = std::array::from_fn(|i| t[tet[i]]);
        let tpe: [f64; 4] = std::array::from_fn(|i| tp[tet[i]]);
        let terms = implexity_physics_solid::material::MaterialLaw::step_energy(
            &m.materials[0],
            &m.materials[1],
            x[s.nc + 3 + o],
            density,
            stiffness,
            &te,
            &tpe,
            &cur[e].strain,
            &prev[e].strain,
        );
        for (key, value) in terms {
            *totals.entry(key.trim_end_matches("_m3").to_string()).or_insert(0.0) +=
                value.iter().sum::<f64>() * volume / 4.0;
        }
    }
    Some(json!(totals))
}

type KernelCache = Mutex<Vec<(String, Arc<ConjugateKernel>)>>;

fn cache() -> &'static KernelCache {
    static CACHE: std::sync::OnceLock<KernelCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}


pub fn kernel(p: &Value) -> CaeResult<Arc<ConjugateKernel>> {
    let key = crate::unified_history::kernel::serialise_problem(p);
    {
        let mut c = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(i) = c.iter().position(|(k, _)| *k == key) {
            let entry = c.remove(i);
            let out = Arc::clone(&entry.1);
            c.push(entry);
            return Ok(out);
        }
    }
    let built = Arc::new(ConjugateKernel::new(p.clone())?);
    let mut c = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if c.len() >= 3 {
        c.remove(0);
    }
    c.push((key, Arc::clone(&built)));
    Ok(built)
}


pub fn conjugate_history_starter(heated_face: (usize, &str)) -> CaeResult<Value> {
    if heated_face.0 > 2 || !matches!(heated_face.1, "lo" | "hi") {
        return contract("heated_face must be (axis 0|1|2, side lo|hi)");
    }
    if heated_face == (2, "hi") {
        return contract("the starter interface is the solid z-high face; heat another solid face");
    }
    let grid = [2usize, 1, 1];
    let times = [0.0, 1.0];
    let temperature = 300.0;
    let mut solid = implexity_physics_solid::solid_history::solid_history_starter();
    for (k, v) in [
        ("name", json!("Synthetic conjugate solid, replace material data before use")),
        ("grid", json!(grid)),
        ("times_s", json!(times)),
        ("temperature_initial_K", json!(temperature)),
        ("temperature_bcs", json!([])),
        ("tractions", json!([])),
        ("heat_fluxes", json!([{"axis": heated_face.0, "side": heated_face.1, "values": [0.0, 1e4]}])),
    ] {
        solid[k] = v;
    }
    Ok(json!({
        "name": "Synthetic conjugate channel, match grids and replace material data before use",
        "components": {"solid": "inelastic_solid_history_block", "fluid": "mac_fluid_history",
            "thermal_contact": "conservative_thermal_contact", "fluid_traction": "conservative_fluid_traction"},
        "solid": solid,
        "fluid": crate::incompressible_transport::fluid_history_starter(grid, &times, temperature, Some((2, "lo"))),
        "interface": {"axis": 2, "solid_side": "hi", "contact_resistance_m2K_W": 0.0,
            "max_displacement_over_cell": MAX_INTERFACE_DISPLACEMENT_OVER_CELL,
            "provenance": "Perfect bonded contact assumed for the synthetic starter"},
        "numerics": {"tolerance": 1e-8, "max_iterations": 30},
    }))
}

fn arr(values: Vec<f64>, shape: &[usize]) -> CaeResult<FieldValue> {
    ArrayD::from_shape_vec(IxDyn(shape), values)
        .map(FieldValue::Array)
        .map_err(|e| CaeError::contract(format!("internal array shape error: {e}")))
}

fn problem_value(problem: &ProviderProblem) -> CaeResult<&Value> {
    problem.downcast_ref::<Value>().ok_or_else(|| {
        CaeError::contract("native_conjugate_history requires its own normalised problem mapping")
    })
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NativeConjugateHistoryProvider;

impl NativeConjugateHistoryProvider {
    pub const IMPLEMENTATION: &'static str =
        "implexity.physics_library.conjugate_history.NativeConjugateHistoryProvider";

    #[must_use]
    pub fn editor_schema(problem: Option<&Value>) -> Value {
        let schema = problem.filter(|p| p.is_object()).map_or_else(
            || json!({}),
            |p| {
                implexity_physics_solid::solid_history::provider_editor_schema(
                    p.get("solid").unwrap_or(&Value::Null),
                )
            },
        );
        let mut properties = Map::new();
        if schema.as_object().is_some_and(|m| !m.is_empty()) {
            properties.insert("solid".into(), schema);
        }
        properties.insert("fluid".into(), crate::incompressible_transport::fluid_editor_schema());
        properties.insert("initialization".into(), crate::conjugate_preload::initialization_editor_schema());
        json!({"properties": properties})
    }

    #[must_use]
    pub fn study_templates(problem: Option<&Value>) -> Vec<Value> {
        match problem.filter(|p| p.is_object()) {
            Some(p) => implexity_physics_solid::solid_history::solid_study_templates(
                p.get("solid").unwrap_or(&Value::Null),
                &["solid"],
            ),
            None => Vec::new(),
        }
    }

    #[must_use]
    pub fn runtime_support_map() -> Map<String, Value> {
        json!({"status": "native_field_solver", "history": true, "data": "user_required", "limitations": LIMITATIONS})
            .as_object()
            .cloned()
            .unwrap_or_default()
    }

    #[must_use]
    pub fn component_slots_map() -> Map<String, Value> {
        let mut out = Map::new();
        for (key, kind) in KINDS {
            out.insert(key.into(), json!({"component_kind": kind, "required": true, "integration": "same_residual_and_complete_history_adjoint"}));
        }
        out.insert("solid.material_history".into(), json!({"component_kind": "material_state_evolution", "required": false, "integration": "current_constitutive_residual_and_energy_with_full_history_adjoint"}));
        out.insert("solid.viscoelasticity".into(), json!({"component_kind": "viscoelastic_solid", "required": false, "integration": "native_branch_state_stress_heat_in_coupled_residual"}));
        out.insert("solid.fatigue_observer".into(), json!({"component_kind": "fatigue_history_observer", "required": false, "integration": "postprocess_solved_solid_history_no_gradient_or_feedback"}));
        out
    }


    pub fn declaration(p: &Value) -> CaeResult<CouplingDeclaration> {
        use implexity_physics_solid::coupling::edge;
        let solid = p.get("solid").cloned().unwrap_or_else(|| json!({}));
        let components = solid.get("components").cloned().unwrap_or_else(|| json!({}));
        let reversible = components.get("material") == Some(&json!("constant_strain_thermoelastic_solid"));
        let reciprocal = reversible
            || ["plasticity", "creep"].iter().any(|k| components.get(*k).is_some_and(|v| !v.is_null()))
            || solid.get("viscoelasticity").is_some_and(|v| !v.is_null());
        let mut edges = vec![
            edge(
                "flow",
                "thermal",
                "wall_heat_flux",
                "monolithic",
                "fluid enthalpy and conservative solid/fluid contact flux",
            ),
            edge(
                "thermal",
                "flow",
                "wall_temperature",
                "monolithic",
                "temperature-dependent viscosity and fluid energy in same state",
            ),
            edge(
                "thermal",
                "structure",
                "temperature_field",
                "monolithic",
                "temperature-dependent constitutive laws and thermal strain",
            ),
            edge(
                "flow",
                "structure",
                "pressure_and_shear_load",
                "monolithic",
                "absolute pressure and MAC wall shear from solved fluid field",
            ),
        ];
        if reciprocal {
            edges.push(edge(
                "structure",
                "thermal",
                if reversible { "reversible_thermoelastic_heat" } else { "inelastic_dissipation_heat" },
                "monolithic",
                if reversible {
                    "explicit Helmholtz entropy coupling"
                } else {
                    "selected inelastic/viscoelastic heat"
                },
            ));
        }
        let strings = |items: &[&str]| items.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let base = CouplingDeclaration {
            provider: NAME.into(),
            active_physics: strings(&["flow", "thermal", "structure"]),
            ports: Vec::new(),
            edges,
            closed_loops: if reciprocal {
                vec![strings(&["flow", "thermal", "structure"])]
            } else {
                vec![strings(&["flow", "thermal"])]
            },
            intentionally_frozen: Vec::new(),
            notes: strings(&LIMITATIONS),
        };
        let with_material = implexity_physics_solid::coupling::with_material_couplings(
            base,
            solid.get("material_history").unwrap_or(&Value::Null),
        )?;
        Ok(implexity_physics_solid::polymer::with_environmental_maxwell_couplings(with_material, &solid))
    }


    pub fn parts(
        &self,
        problem: &Value,
        design: &NamedArrays,
    ) -> CaeResult<(Value, Arc<ConjugateKernel>, Vec<f64>)> {
        let p = normalise(problem)?;
        let k = kernel(&p)?;
        let names = design.names();
        if names.len() != 3 || COORDS.iter().any(|c| !design.contains(c)) {
            return contract("all three native coordinate families required");
        }
        let get = |name: &str| {
            design.get(name).map(|a| (a.shape().to_vec(), a.iter().copied().collect::<Vec<f64>>()))
        };
        let (Some((rs, rho)), Some((hs, h)), Some((cs, c))) =
            (get(COORDS[0]), get(COORDS[1]), get(COORDS[2]))
        else {
            return contract("all three native coordinate families required");
        };
        let grid = k.grid.to_vec();
        if rs != grid || cs != grid || hs != [3] || rho.iter().chain(&h).chain(&c).any(|v| !v.is_finite()) {
            return contract("invalid native conjugate design shapes/nonfinite values");
        }
        if rho.iter().any(|v| *v < 0.0 || *v > 1.0)
            || k.sidx.iter().any(|i| rho[*i] <= 0.0)
            || h.iter().any(|v| *v <= 0.0)
            || c.iter().any(|v| *v < 0.0 || *v > 1.0)
        {
            return contract("invalid conjugate design bounds");
        }
        let mut x = rho;
        x.extend(h);
        x.extend(c);
        Ok((p, k, x))
    }

    fn split(g: &[f64], k: &ConjugateKernel) -> CaeResult<NamedArrays> {
        let grid = k.grid.to_vec();
        let to = |v: Vec<f64>, shape: &[usize]| {
            ArrayD::from_shape_vec(IxDyn(shape), v).map_err(|e| CaeError::contract(e.to_string()))
        };
        let mut out = NamedArrays::new();
        out.insert(COORDS[0], to(g[..k.nc].to_vec(), &grid)?);
        out.insert(COORDS[1], to(g[k.nc..k.nc + 3].to_vec(), &[3])?);
        out.insert(COORDS[2], to(g[k.nc + 3..].to_vec(), &grid)?);
        Ok(out)
    }

    fn registrations(k: &ConjugateKernel, h: &[f64]) -> CaeResult<(Value, Value, Value)> {
        let make = |shape: [usize; 3], offset: [f64; 3]| -> CaeResult<Value> {
            let lo: [f64; 3] = std::array::from_fn(|a| (offset[a] - 0.5) * h[a]);
            let hi: [f64; 3] = std::array::from_fn(|a| (offset[a] + shape[a] as f64 - 0.5) * h[a]);
            implexity_geometry::field_registration::axis_aligned_registration(shape, lo, hi, "cell")
                .map(|r| r.to_wire())
                .map_err(|e| CaeError::contract(e.to_string()))
        };
        Ok((make(k.grid, [0.0; 3])?, make(k.s.grid, k.soffset)?, make(k.f.grid, k.foffset)?))
    }


    #[allow(clippy::too_many_lines)]
    pub fn evaluate_named(&self, problem: &Value, design: &NamedArrays) -> CaeResult<Evaluation> {
        let (_p, k, x) = self.parts(problem, design)?;
        let sol = k.solve(&x)?;
        let s = &k.s;
        let f = &k.f;
        let nt = k.nt;
        let sl = k.solid_slice.clone();
        let fl = k.fluid_slice.clone();
        let sx = k.solid_design(&x);
        let h: Vec<f64> = design.get(COORDS[1]).map(|a| a.iter().copied().collect()).unwrap_or_default();
        let mut dg = k.diagnostics(&x, &sol, &design_identity(design)?)?;
        let (reg, sreg, freg) = Self::registrations(&k, &h)?;
        dg.insert("field_registration".into(), reg.clone());
        dg.insert("design_field_registrations".into(), json!({COORDS[0]: reg, COORDS[2]: reg}));
        let states = &sol.states;
        let sd = s.observe(nt - 1, &states[nt - 1][sl.clone()], &states[nt - 2][sl.clone()], &sx);
        let ff = f.fields(&states[nt - 1][fl.clone()]);
        let cell = |values: &[f64], width: usize| -> Vec<f64> {
            let mut out = vec![0.0; s.nc * width];
            for c in 0..s.nc {
                for j in 0..width {
                    out[c * width + j] = (0..6).map(|t| values[(6 * c + t) * width + j]).sum::<f64>() / 6.0;
                }
            }
            out
        };
        let sgrid = s.grid.to_vec();
        let fgrid = f.grid.to_vec();
        let mut fields: BTreeMap<String, FieldValue> = BTreeMap::new();
        let mut put = |name: &str, v: FieldValue| {
            fields.insert(name.to_string(), v);
        };
        let grid = k.grid.to_vec();
        put(
            "design_control",
            arr(design.get(COORDS[0]).map(|a| a.iter().copied().collect()).unwrap_or_default(), &grid)?,
        );
        put(
            "design_material",
            arr(design.get(COORDS[2]).map(|a| a.iter().copied().collect()).unwrap_or_default(), &grid)?,
        );
        let tmean: Vec<f64> = s
            .mesh
            .tets
            .iter()
            .map(|t| t.iter().map(|n| sd.temperature_nodes[*n]).sum::<f64>() / 4.0)
            .collect();
        put("solid_temperature_K", arr(cell(&tmean, 1), &sgrid)?);
        put("solid_von_mises_Pa", arr(cell(&sd.von_mises, 1), &sgrid)?);
        put("solid_equivalent_plastic_strain", arr(cell(&sd.equivalent_plastic, 1), &sgrid)?);
        put("solid_equivalent_creep_strain", arr(cell(&sd.equivalent_creep, 1), &sgrid)?);
        put("fluid_temperature_K", arr(ff.temperature.clone(), &fgrid)?);
        put("fluid_pressure_absolute_Pa", arr(ff.pressure.iter().map(|p| f.pref + p).collect(), &fgrid)?);
        let mut vshape = fgrid.clone();
        vshape.push(3);
        put("fluid_velocity_m_s", arr(ff.velocity.iter().flatten().copied().collect(), &vshape)?);
        put(
            "state_history_nondimensional",
            arr(states.iter().flatten().copied().collect(), &[nt, k.state_size])?,
        );
        put("times_s", arr(s.times.clone(), &[nt])?);
        put(
            "solid_temperature_nodes_history_K",
            arr(
                states.iter().enumerate().flat_map(|(n, z)| s.nodal_temperature(n, &z[sl.clone()])).collect(),
                &[nt, s.nn],
            )?,
        );
        let mut fshape = vec![nt];
        fshape.extend(&fgrid);
        put(
            "fluid_temperature_history_K",
            arr(states.iter().flat_map(|z| f.fields(&z[fl.clone()]).temperature).collect(), &fshape)?,
        );
        let ne = s.ne;
        put(
            "solid_tetrahedral_stress_mandel_Pa",
            arr(sd.stress.iter().flatten().copied().collect(), &[ne, 6])?,
        );
        let start = k.service_start_index;
        put(
            "service_times_s",
            arr(s.times[start..].iter().map(|t| t - s.times[start]).collect(), &[nt - start])?,
        );
        put(
            "service_state_history_nondimensional",
            arr(states[start..].iter().flatten().copied().collect(), &[nt - start, k.state_size])?,
        );
        put("service_initial_state_nondimensional", arr(states[start].clone(), &[k.state_size])?);
        put("interface_times_s", arr(s.times[1..].to_vec(), &[nt - 1])?);
        put(
            "solid_mesh_nodes_m",
            arr(
                {
                    let (off, hh) = (k.soffset, [h[0], h[1], h[2]]);
                    s.mesh
                        .ijk
                        .iter()
                        .flat_map(|q| (0..3).map(move |a| (q[a] as f64 - 0.5 + off[a]) * hh[a] * 1e-3))
                        .collect()
                },
                &[s.nn, 3],
            )?,
        );
        put(
            "solid_displacement_nodes_history_m",
            arr(
                states
                    .iter()
                    .enumerate()
                    .flat_map(|(n, z)| s.nodal_displacement(n, &z[sl.clone()]).into_iter().flatten())
                    .collect(),
                &[nt, s.nn, 3],
            )?,
        );
        put(
            "solid_tetrahedron_nodes",
            arr(s.mesh.tets.iter().flatten().map(|v| *v as f64).collect(), &[ne, 4])?,
        );
        put("solid_tetrahedron_native_cell", arr(s.mesh.owners.iter().map(|v| *v as f64).collect(), &[ne])?);
        for (a, face) in ff.faces.iter().enumerate() {
            put(
                &format!("fluid_velocity_{}_faces_m_s", ["x", "y", "z"][a]),
                arr(face.clone(), &f.map_shapes[a])?,
            );
        }
        let m = &s.model;
        let mut solid_history_metadata: Map<String, Value> = Map::new();
        let blocks: Vec<(&str, Option<Vec<Value>>, std::ops::Range<usize>, Vec<&str>)> = vec![
            (
                "material",
                m.history.as_ref().map(|h| h.metadata.clone()),
                m.layout.material_start()..s.internal_size,
                vec!["stored_energy_J_m3", "conductivity_W_mK", "yield_stress_Pa"],
            ),
            (
                "viscoelastic",
                m.viscoelastic.as_ref().map(|v| v.metadata()),
                m.layout.viscoelastic(),
                vec![
                    "stored_energy_J_m3",
                    "heat_increment_J_m3",
                    "assembled_heat_increment_J_m3",
                    "assembled_numerical_dissipation_increment_J_m3",
                ],
            ),
        ];
        for (prefix, metadata, part, names) in blocks {
            let Some(state_metadata) = metadata else { continue };
            let key = format!("solid_{prefix}_state_history");
            let width = part.len();
            let mut data = Vec::new();
            for (n, z) in states.iter().enumerate() {
                for fe in s.fields(n, &z[sl.clone()], &sx) {
                    data.extend_from_slice(&fe.state[part.clone()]);
                }
            }
            put(&key, arr(data, &[nt, ne, width])?);
            let units: std::collections::BTreeSet<&str> =
                state_metadata.iter().map(|r| r["units"].as_str().unwrap_or_default()).collect();
            solid_history_metadata.insert(key, json!({"units": if units.len() == 1 { state_metadata[0]["units"].clone() } else { json!("mixed_explicit_states") },
                "association": "material_point_history", "axes": ["time", "tetrahedron", "state"], "rank": "scalar",
                "state_metadata": state_metadata, "source": "native_coupled_solid_internal_state"}));
            for name in names {
                let key = format!("solid_{prefix}_{name}");
                let values = match (prefix, name) {
                    ("material", "stored_energy_J_m3") => sd.material_stored_energy.clone(),
                    ("material", "conductivity_W_mK") => sd.conductivity.clone(),
                    ("material", "yield_stress_Pa") => sd.yield_stress.clone(),
                    (_, n) => sd.polymer_column(n),
                };
                put(&key, arr(cell(&values, 1), &sgrid)?);
                let units = if name.ends_with("J_m3") {
                    "J/m^3"
                } else if name.ends_with("W_mK") {
                    "W/(m*K)"
                } else {
                    "Pa"
                };
                solid_history_metadata.insert(
                    key,
                    json!({"units": units, "association": "cell", "registration": sreg,
                    "rank": "scalar", "source": "volume_average_of_native_solid_tetrahedral_fields"}),
                );
            }
        }
        let ints = k.interface_history(states, &x)?;
        let q_last = ints.last().map(|i| i.heat_flux.clone()).unwrap_or_default();
        let count = q_last.len();
        put("interface_heat_flux_W_m2", arr(q_last, &[count])?);
        put(
            "interface_traction_on_solid_Pa",
            arr(
                ints.last()
                    .map(|i| i.traction_on_solid.iter().flatten().copied().collect())
                    .unwrap_or_default(),
                &[count, 3],
            )?,
        );
        put(
            "interface_heat_flux_history_W_m2",
            arr(ints.iter().flat_map(|i| i.heat_flux.clone()).collect(), &[nt - 1, count])?,
        );
        let mut metadata = Map::new();
        for name in fields.keys() {
            let n = name.as_str();
            let units = if n.contains("heat_flux") {
                "W/m^2"
            } else if n.ends_with("_m_s") {
                "m/s"
            } else if n.ends_with("_Pa") {
                "Pa"
            } else if n.ends_with("_K") {
                "K"
            } else if n == "times_s" {
                "s"
            } else {
                "1"
            };
            let mut row = json!({"units": units, "association": "exact_history_or_interface", "source": "native_coupled_field_solution", "rank": "scalar"});
            let mut set = |pairs: Value| {
                if let (Some(r), Some(p)) = (row.as_object_mut(), pairs.as_object()) {
                    for (a, b) in p {
                        r.insert(a.clone(), b.clone());
                    }
                }
            };
            if matches!(n, "service_times_s" | "interface_times_s") {
                set(json!({"units": "s", "association": "time"}));
            }
            if n == "service_state_history_nondimensional" {
                set(
                    json!({"time_field": "service_times_s", "axes": ["time", "coupled_state"], "source": "exact_slice_of_full_coupled_history_no_reset"}),
                );
            }
            if n == "service_initial_state_nondimensional" {
                set(json!({"association": "coupled_state", "source_state_index": start,
                    "source": if start > 0 { "resolved_preload_endpoint" } else { "prescribed_seed" }}));
            }
            if matches!(n, "design_control" | "design_material") {
                set(json!({"association": "cell", "registration": reg}));
            }
            if matches!(
                n,
                "solid_temperature_K"
                    | "solid_von_mises_Pa"
                    | "solid_equivalent_plastic_strain"
                    | "solid_equivalent_creep_strain"
            ) {
                set(
                    json!({"association": "cell", "registration": sreg, "source": "volume_average_of_native_tetrahedral_fields"}),
                );
            }
            if matches!(n, "fluid_temperature_K" | "fluid_pressure_absolute_Pa" | "fluid_velocity_m_s") {
                set(json!({"association": "cell", "registration": freg}));
            }
            if n == "fluid_velocity_m_s" {
                set(
                    json!({"rank": "vector", "components": ["x", "y", "z"], "source": "MAC_face_average_not_displacement"}),
                );
            }
            if n == "interface_traction_on_solid_Pa" {
                set(
                    json!({"association": "interface_quadrature", "rank": "vector", "components": ["x", "y", "z"]}),
                );
            }
            if n == "solid_tetrahedral_stress_mandel_Pa" {
                set(
                    json!({"association": "tetrahedron", "rank": "tensor", "components": ["xx", "yy", "zz", "sqrt(2)yz", "sqrt(2)xz", "sqrt(2)xy"], "tensor_convention": "orthonormal_Mandel"}),
                );
            }
            if n == "solid_mesh_nodes_m" {
                set(
                    json!({"units": "m", "association": "node", "rank": "vector", "components": ["x", "y", "z"], "configuration": "reference", "source": "native_solid_mesh_with_partition_offset"}),
                );
            }
            if n == "solid_displacement_nodes_history_m" {
                set(
                    json!({"units": "m", "association": "node_history", "axes": ["time", "node", "component"], "rank": "vector", "components": ["x", "y", "z"], "reference_coordinate_field": "solid_mesh_nodes_m", "source": "native_solid_displacement_DOFs"}),
                );
            }
            if n == "solid_tetrahedron_nodes" {
                set(
                    json!({"association": "tetrahedron_connectivity", "index_base": 0, "node_field": "solid_mesh_nodes_m"}),
                );
            }
            if n == "solid_tetrahedron_native_cell" {
                set(
                    json!({"association": "tetrahedron", "index_base": 0, "cell_order": "C", "cell_grid": s.grid, "registration": sreg}),
                );
            }
            if n.starts_with("interface_heat_flux") {
                set(
                    json!({"association": if n.contains("history") { "interface_quadrature_history" } else { "interface_quadrature" }}),
                );
            }
            if n == "interface_heat_flux_history_W_m2" {
                set(json!({"time_field": "interface_times_s"}));
            }
            if n.contains("_faces_") {
                let axis = ["x", "y", "z"].iter().position(|a| n.split('_').nth(2) == Some(*a)).unwrap_or(0);
                set(json!({"association": "face", "face_axis": axis, "source": "exact_MAC_velocity_DOFs"}));
            }
            if let Some(extra) = solid_history_metadata.get(n) {
                set(extra.clone());
            }
            metadata.insert(name.clone(), row);
        }
        dg.insert("field_metadata".into(), Value::Object(metadata));
        let (values, _) = k.responses(states, &x, None)?;
        let responses = RESPONSES.iter().map(|r| (*r).to_string()).zip(values).collect();
        Ok(Evaluation { provider: NAME.into(), responses, diagnostics: dg, fields })
    }


    pub fn sensitivities_named(
        &self,
        problem: &Value,
        design: &NamedArrays,
        responses: &[String],
    ) -> CaeResult<DesignSensitivities> {
        let (_p, k, x) = self.parts(problem, design)?;
        let unique: std::collections::BTreeSet<&String> = responses.iter().collect();
        if responses.is_empty()
            || unique.len() != responses.len()
            || responses.iter().any(|r| !RESPONSES.contains(&r.as_str()))
        {
            return contract("invalid conjugate response set");
        }
        let sol = k.solve(&x)?;
        k.certify_sensitivity(&sol, &x)?;
        let (values, partials) = k.responses(&sol.states, &x, Some(responses))?;
        let (gu, gx) = partials.ok_or_else(|| CaeError::contract("conjugate response partials missing"))?;
        let out = k.assembly.adjoint_many(&x, &sol, &gu, &gx, None)?;
        let mut dg = k.diagnostics(&x, &sol, &design_identity(design)?)?;
        let h: Vec<f64> = design.get(COORDS[1]).map(|a| a.iter().copied().collect()).unwrap_or_default();
        let (reg, _, _) = Self::registrations(&k, &h)?;
        dg.insert("field_registration".into(), reg.clone());
        dg.insert("design_field_registrations".into(), json!({COORDS[0]: reg, COORDS[2]: reg}));
        dg.insert("adjoint_factorizations".into(), json!(out.adjoint_factorizations));
        dg.insert("adjoint_factorization_builds".into(), json!(out.adjoint_factorization_builds));
        dg.insert("adjoint_factorization_reuses".into(), json!(out.adjoint_factorization_reuses));
        dg.insert(
            "maximum_transpose_relative_residual".into(),
            json!(out.maximum_transpose_relative_residual),
        );
        dg.insert("history_states_retained".into(), json!(out.history_states_retained));
        dg.insert("history_derivative".into(), json!(out.history_derivative));
        let mut result = DesignSensitivities { diagnostics: dg, ..DesignSensitivities::default() };
        let m = responses.len();
        for (j, name) in responses.iter().enumerate() {
            let i = RESPONSES.iter().position(|r| r == name).unwrap_or(0);
            result.responses.insert(name.clone(), values[i]);
            let g: Vec<f64> = (0..out.gradients.nrows).map(|r| out.gradients.data[r * m + j]).collect();
            result.gradients.insert(name.clone(), Self::split(&g, &k)?);
        }
        Ok(result)
    }
}

impl CaeProvider for NativeConjugateHistoryProvider {
    fn name(&self) -> &str {
        NAME
    }

    fn implementation(&self) -> &str {
        Self::IMPLEMENTATION
    }

    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        let mut caps = LegacySingleArrayProviderCapabilities::new(
            NAME,
            ["flow", "thermal", "structure", "plasticity", "creep"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            RESPONSES.iter().map(|s| (*s).to_string()).collect(),
        );
        caps.base.fields = [
            "fluid_velocity_m_s",
            "fluid_temperature_K",
            "solid_temperature_K",
            "solid_equivalent_plastic_strain",
            "interface_heat_flux_W_m2",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        caps.base.sensitivities = true;
        caps.base.nonlinear = true;
        caps.base.design_coordinates = COORDS.iter().map(|s| (*s).to_string()).collect();
        caps.editor = json!({"kind": "native_json", "title": "Conjugate flow and inelastic history",
            "required_packages": ["conjugate_heat_mechanics", "solid_mechanics", "inelastic_materials", "incompressible_transport", "conservative_interfaces"],
            "problem_template": conjugate_history_starter((2, "lo"))?})
        .as_object()
        .cloned()
        .unwrap_or_default();
        caps.base.notes = LIMITATIONS.iter().map(|s| (*s).to_string()).collect();
        Ok(ProviderCapabilities::Legacy(Box::new(caps)))
    }

    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        Ok(Arc::new(normalise(problem)?))
    }

    fn preflight(
        &self,
        problem: &ProviderProblem,
        _topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        let p = normalise(problem_value(problem)?)?;
        Ok(json!({"ok": true, "requires_complete_design": true, "physical_qualification": false,
            "initialization": crate::conjugate_preload::initialization_description(&p), "limitations": LIMITATIONS})
        .as_object()
        .cloned()
        .unwrap_or_default())
    }

    fn evaluate(&self, _problem: &ProviderProblem, _topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        Err(CaeError::contract("'NativeConjugateHistoryProvider' object has no attribute 'evaluate'"))
    }

    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        _topology: &ArrayD<f64>,
        _response: &str,
    ) -> CaeResult<Sensitivity> {
        Err(CaeError::contract("'NativeConjugateHistoryProvider' object has no attribute 'sensitivity'"))
    }

    fn coupling_declaration(&self, problem: Option<&ProviderProblem>) -> Option<CaeResult<Value>> {
        let p = problem.and_then(|p| p.downcast_ref::<Value>()).cloned().unwrap_or_else(|| json!({}));
        Some(Self::declaration(&p).map(|d| d.to_value()))
    }

    fn coupling_validation(
        &self,
        problem: Option<&ProviderProblem>,
        for_optimization: bool,
    ) -> Option<Result<Value, String>> {
        let p = problem.and_then(|p| p.downcast_ref::<Value>())?;
        let run = || -> CaeResult<Value> {
            let declaration = Self::declaration(&normalise(p)?)?;
            let rules = implexity_core::registries::global().extensions.coupling_rules();
            Ok(validate_declaration(&declaration, &rules, for_optimization))
        };
        Some(run().map_err(|e| e.message().to_string()))
    }

    fn runtime_support(&self) -> Option<Map<String, Value>> {
        Some(Self::runtime_support_map())
    }

    fn component_slots(&self) -> Option<Map<String, Value>> {
        Some(Self::component_slots_map())
    }

    fn interface(&self, name: &str) -> Option<&(dyn Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(name)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl DesignOperations for NativeConjugateHistoryProvider {
    fn provides(&self, op: DesignOp) -> bool {
        matches!(
            op,
            DesignOp::EvaluateDesign
                | DesignOp::PreflightDesign
                | DesignOp::SensitivityDesign
                | DesignOp::SensitivitiesDesign
        )
    }

    fn evaluate_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        _operating_point: usize,
    ) -> CaeResult<Evaluation> {
        self.evaluate_named(problem_value(problem)?, design)
    }

    fn preflight_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> CaeResult<Map<String, Value>> {
        let (_p, k, _x) = self.parts(problem_value(problem)?, design)?;
        Ok(json!({"ok": true, "issues": [], "state_unknowns_per_step": k.state_size, "coupled_assembly": k.assembly.report(),
            "physical_qualification": false, "initialization": k.initialization, "limitations": LIMITATIONS})
        .as_object()
        .cloned()
        .unwrap_or_default())
    }

    fn sensitivity_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        response: &str,
        _operating_point: usize,
    ) -> CaeResult<DesignSensitivity> {
        let mut out = self.sensitivities_named(problem_value(problem)?, design, &[response.to_string()])?;
        Ok(DesignSensitivity {
            value: out.responses.get(response).copied().unwrap_or(f64::NAN),
            gradients: out.gradients.remove(response).unwrap_or_default(),
            diagnostics: out.diagnostics,
        })
    }

    fn sensitivities_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        responses: &[String],
        _operating_point: usize,
    ) -> CaeResult<DesignSensitivities> {
        self.sensitivities_named(problem_value(problem)?, design, responses)
    }

    fn workspace_declaration(&self, name: &str, problem: Option<&Value>) -> Option<CaeResult<Value>> {
        match name {
            "editor_schema" => Some(Ok(Self::editor_schema(problem))),
            "study_templates" => Some(Ok(Value::Array(Self::study_templates(problem)))),
            _ => None,
        }
    }
}


pub fn install_conjugate_heat_mechanics(ctx: &InstallContext<'_>) -> CaeResult<()> {
    implexity_physics_base::coupling_policy::install(ctx, crate::addins::CONJUGATE_HEAT_MECHANICS)?;
    ctx.register_provider(Arc::new(NativeConjugateHistoryProvider))?;
    Ok(())
}
