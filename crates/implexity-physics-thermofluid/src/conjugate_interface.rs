// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use serde_json::Value;

use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use implexity_physics_solid::solid_face_trace::{Side, SolidFaceTrace};
use implexity_physics_solid::solid_history::SolidKernel;
use implexity_solve::coupled_history::HistoryInterface;
use implexity_solve::local_assembly::{Kind, LocalResidual};
use implexity_solve::matrix::Jacobian;

use crate::conservative_interfaces::{ConservativeFluidTraction, ConservativeThermalContact};
use crate::incompressible_transport::{FluidKernel, FluidLaw, flat};
use crate::local_group::{Group, GroupOps, StepKernel, typed_group};

fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}

const WIDTH: usize = 15;
const ROWS: usize = 17;

struct ContactKernel {
    axis: usize,
    tangential: [usize; 2],
    normal: [f64; 3],
    contact: f64,
    law: FluidLaw,
    fluid_t0: f64,
    fluid_ts: f64,
    fluid_hs: f64,
    ps: f64,
    pref: f64,
    us: f64,
    solid_t0: f64,
    solid_ts: f64,
    thermal_scale: f64,
    force_scale: f64,
    edge_weights: Vec<[f64; 4]>,
    trace_weights: Vec<[f64; 4]>,
    prescribed: Arc<Vec<[f64; 4]>>,
    nodes: Vec<[usize; 4]>,
    fixed_t: Arc<Vec<Vec<f64>>>,
}

struct Physical<S> {
    q: S,
    area: S,
    traction: [S; 3],
    solid_temperature: S,
    fluid_temperature: S,
    pressure: S,
    resistance: S,
}

impl ContactKernel {
    fn physical<S: Scalar>(&self, item: usize, z: &[S], x: &[S]) -> Physical<S> {
        let h: [S; 3] = std::array::from_fn(|a| x[a] * 1e-3);
        let axis = self.axis;
        let area = h[0] * h[1] * h[2] / (h[axis] * 6.0);
        let w = &self.trace_weights[item];
        let mut trace = S::zero();
        for i in 0..4 {
            trace += z[i] * w[i];
        }
        let ts = trace * self.solid_ts + self.solid_t0;
        let tf = z[4] * self.fluid_ts + self.fluid_t0;
        let (mu, k, _) = self.law.properties(tf);
        let ke = k * self.law.fraction(x[3]);
        let resistance = h[axis] * 0.5 / ke + self.contact;
        let q = ConservativeThermalContact::eliminated_conductive_flux(ts, tf, resistance);
        let pressure = z[5] * self.ps + self.pref;
        let mut edge = [S::zero(); 4];
        for j in 0..4 {
            let tn = z[10 + j] * self.fluid_ts + self.fluid_t0;
            let mue = (mu + self.law.properties(tn).0) * 0.5;
            edge[j] = mue * 2.0 * self.edge_weights[item][j] * self.us * z[6 + j] / h[axis];
        }
        let mut shear = [S::zero(); 3];
        for (b, t) in self.tangential.iter().enumerate() {
            shear[*t] = (edge[2 * b] + edge[2 * b + 1]) * 0.5;
        }
        let normal_gradient = z[14] * (self.normal[axis] * self.us) / h[axis];
        let viscous: [S; 3] = std::array::from_fn(|a| shear[a] + mu * 2.0 * normal_gradient * self.normal[a]);
        let tau: [S; 3] = std::array::from_fn(|a| S::from_f64(self.normal[a]));
        let traction = ConservativeFluidTraction::traction(pressure, &tau, &viscous);
        Physical { q, area, traction, solid_temperature: ts, fluid_temperature: tf, pressure, resistance }
    }
}

impl LocalResidual for ContactKernel {
    fn residual<S: Scalar>(&self, item: usize, current: &[S], _previous: &[S], design: &[S], out: &mut [S]) {
        let p = self.physical(item, current, design);
        let w = &self.trace_weights[item];
        for i in 0..4 {
            out[i] = p.q * p.area * w[i] / self.thermal_scale;
        }
        out[4] = -(p.q * p.area) / self.fluid_hs;
        for i in 0..4 {
            for a in 0..3 {
                out[5 + 3 * i + a] = -(p.traction[a] * p.area * w[i]) / self.force_scale;
            }
        }
    }
}

impl StepKernel for ContactKernel {
    fn prescribed_state(&self, n: usize, current: &mut [f64], previous: &mut [f64]) {
        for (e, nodes) in self.nodes.iter().enumerate() {
            for (i, node) in nodes.iter().enumerate() {
                current[e * WIDTH + i] = (self.fixed_t[n][*node] - self.solid_t0) / self.solid_ts;
                previous[e * WIDTH + i] =
                    (self.fixed_t[n.saturating_sub(1)][*node] - self.solid_t0) / self.solid_ts;
            }
        }
        let _ = &self.prescribed;
    }
}

#[derive(Debug, Clone)]
pub struct InterfaceObservables {
    pub heat_flux: Vec<f64>,
    pub area: Vec<f64>,
    pub solid_trace_temperature: Vec<f64>,
    pub fluid_cell_temperature: Vec<f64>,
    pub fluid_wall_temperature: Vec<f64>,
    pub fluid_absolute_pressure: Vec<f64>,
    pub traction_on_solid: Vec<[f64; 3]>,
    pub solid_nodal_load: Vec<[f64; 3]>,
    pub fluid_wall_reaction: Vec<[f64; 3]>,
    pub solid_outward_power: Vec<f64>,
    pub fluid_outward_power: Vec<f64>,
    pub maximum_temperature_interface_residual: f64,
    pub heat_balance: f64,
    pub force_balance: [f64; 3],
}

pub struct ConjugateInterface {
    group: Group<ContactKernel>,
    pub axis: usize,
    pub normal: [f64; 3],
    pub nodes: Vec<[usize; 4]>,
    pub cells: Vec<[usize; 3]>,
    columns: Vec<i64>,
    maps: Vec<usize>,
    nn: usize,
    contact: f64,
    fluid: Arc<FluidKernel>,
}

impl std::fmt::Debug for ConjugateInterface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConjugateInterface")
            .field("axis", &self.axis)
            .field("items", &self.nodes.len())
            .finish_non_exhaustive()
    }
}

impl ConjugateInterface {

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn new(
        state_size: usize,
        design_size: usize,
        solid_start: usize,
        fluid_start: usize,
        solid: &SolidKernel,
        fluid: &Arc<FluidKernel>,
        solid_xmap: &[usize],
        fluid_xmap: &[usize],
        settings: &Value,
    ) -> CaeResult<Self> {
        let axis = settings["axis"].as_u64().unwrap_or(0) as usize;
        let tangential: [usize; 2] = {
            let t: Vec<usize> = (0..3).filter(|a| *a != axis).collect();
            [t[0], t[1]]
        };
        let side = settings["solid_side"].as_str().unwrap_or("hi");
        let fside = if side == "hi" { "lo" } else { "hi" };
        if fluid.boundary(axis, fside).is_none_or(|b| b.thermal != "interface") {
            return contract("interface fluid face not declared as interface");
        }
        if tangential.iter().any(|a| solid.grid[*a] != fluid.grid[*a]) {
            return contract("current contact adapter requires matching tangential cell counts");
        }
        let trs = solid.thermal_row_slice();
        let drs = solid.displacement_row_slice();
        for (name, rs, count) in
            [("thermal_row_slice", &trs, solid.n_t()), ("displacement_row_slice", &drs, solid.n_u())]
        {
            if rs.end > solid.state_size || rs.len() != count {
                return contract(format!(
                    "conjugate interface requires the solid kernel to declare a unit-step {name} covering its {count} free rows inside state_size={}; declared slice({}, {}, None)",
                    solid.state_size, rs.start, rs.end
                ));
            }
        }
        if !(trs.end <= drs.start || drs.end <= trs.start) {
            return contract(format!(
                "solid thermal_row_slice slice({}, {}, None) and displacement_row_slice slice({}, {}, None) overlap",
                trs.start, trs.end, drs.start, drs.end
            ));
        }
        let mut tmap = vec![-1i64; solid.nn];
        for (k, node) in solid.free_t.iter().enumerate() {
            tmap[*node] = (solid_start + trs.start + k) as i64;
        }
        let mut umap = vec![-1i64; solid.nn * 3];
        for (k, dof) in solid.free_u.iter().enumerate() {
            umap[*dof] = (solid_start + drs.start + k) as i64;
        }
        let nshape = [solid.grid[0] + 1, solid.grid[1] + 1, solid.grid[2] + 1];
        let trace = SolidFaceTrace::new(solid.grid, &solid.mesh.ijk, &solid.mesh.tets, &solid.mesh.owners)?;
        let side_enum = if side == "hi" { Side::Hi } else { Side::Lo };
        let fs = fluid_start as i64;
        let (mut rows, mut cols, mut maps) = (Vec::new(), Vec::new(), Vec::new());
        let (mut nodes_out, mut cells_out, mut edge_data, mut trace_weights) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for fc in fluid.face_cells(axis, fside) {
            let mut base = fc;
            base[axis] = if side == "hi" { solid.grid[axis] } else { 0 };
            let mut nodes = [0usize; 4];
            for (slot, (da, db)) in [(0, 0), (1, 0), (1, 1), (0, 1)].into_iter().enumerate() {
                let mut v = base;
                v[tangential[0]] += da;
                v[tangential[1]] += db;
                nodes[slot] = flat(nshape, v);
            }
            let mut sc = fc;
            sc[axis] = if side == "hi" { solid.grid[axis] - 1 } else { 0 };
            let weights = trace.quadrature(sc, axis, side_enum, nodes)?;
            let mut ids: Vec<i64> = nodes.iter().map(|n| tmap[*n]).collect();
            ids.push(fs + fluid.tid(fc) as i64);
            ids.push(fs + fluid.pid(fc) as i64);
            let (mut vel, mut neighbours, mut edgeweights) = (Vec::new(), Vec::new(), Vec::new());
            for b in tangential {
                for delta in 0..2usize {
                    let mut face = fc;
                    face[b] += delta;
                    let v = fluid.map(b, face);
                    vel.push(if v >= 0 { fs + v } else { -1 });
                    let inside = if delta == 0 { fc[b] >= 1 } else { fc[b] + 1 < fluid.grid[b] };
                    let mut adj = fc;
                    if inside {
                        if delta == 0 {
                            adj[b] -= 1;
                        } else {
                            adj[b] += 1;
                        }
                        edgeweights.push(1.0);
                    } else {
                        let bd = fluid.boundary(b, if delta == 0 { "lo" } else { "hi" });
                        edgeweights.push(if bd.is_some_and(|x| x.pressure) { 0.0 } else { 1.0 });
                    }
                    neighbours.push(fs + fluid.tid(adj) as i64);
                }
            }
            let mut normal_face = fc;
            if fside == "lo" {
                normal_face[axis] += 1;
            }
            let nv = fluid.map(axis, normal_face);
            ids.extend(vel);
            ids.extend(neighbours);
            ids.push(if nv >= 0 { fs + nv } else { -1 });
            let mut row: Vec<i64> = nodes.iter().map(|n| tmap[*n]).collect();
            row.push(fs + fluid.tid(fc) as i64);
            for node in nodes {
                for c in 0..3 {
                    row.push(umap[3 * node + c]);
                }
            }
            let map: Vec<i64> = (0..3)
                .map(|i| solid_xmap[solid.nc + i] as i64)
                .chain(std::iter::once(fluid_xmap[fluid.cell_index(fc)] as i64))
                .collect();
            let ew = [edgeweights[0], edgeweights[1], edgeweights[2], edgeweights[3]];
            for w in weights {
                rows.push(row.clone());
                cols.push(ids.clone());
                maps.push(map.clone());
                nodes_out.push(nodes);
                cells_out.push(fc);
                edge_data.push(ew);
                trace_weights.push(w);
            }
        }
        let mut normal = [0.0; 3];
        normal[axis] = if side == "hi" { 1.0 } else { -1.0 };
        let contact = settings["contact_resistance_m2K_W"].as_f64().unwrap_or(f64::NAN);
        let m = &solid.model;
        let kernel = ContactKernel {
            axis,
            tangential,
            normal,
            contact,
            law: fluid.law.clone(),
            fluid_t0: fluid.t0,
            fluid_ts: fluid.ts,
            fluid_hs: fluid.hs,
            ps: fluid.ps,
            pref: fluid.pref,
            us: fluid.us,
            solid_t0: m.t0,
            solid_ts: m.ts,
            thermal_scale: m.ks * m.ts * m.ls,
            force_scale: m.ss * m.ls * m.ls,
            edge_weights: edge_data,
            trace_weights,
            prescribed: Arc::new(Vec::new()),
            nodes: nodes_out.clone(),
            fixed_t: Arc::new(solid.fixed_t.clone()),
        };
        let batch = solid.p["assembly"]["batch_size"].as_u64().map_or(64, |b| b as usize).max(1);
        let design_rows: Vec<Vec<i64>> = maps.clone();
        let group = typed_group(kernel, &rows, &cols, &design_rows, state_size, design_size, batch)?
            .ok_or_else(|| CaeError::contract("conjugate interface has no interface faces"))?;
        let _ = ROWS;
        Ok(Self {
            group,
            axis,
            normal,
            nodes: nodes_out,
            cells: cells_out,
            columns: cols.into_iter().flatten().collect(),
            maps: maps.into_iter().flatten().map(|v| v as usize).collect(),
            nn: solid.nn,
            contact,
            fluid: Arc::clone(fluid),
        })
    }


    pub fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        self.group.residual(n, z, old, x)
    }


    pub fn observables(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<InterfaceObservables> {
        let k = self.group.kernel();
        let count = self.nodes.len();
        let (current, _) = self.group.prescribed(n);
        let mut out = InterfaceObservables {
            heat_flux: Vec::with_capacity(count),
            area: Vec::with_capacity(count),
            solid_trace_temperature: Vec::with_capacity(count),
            fluid_cell_temperature: Vec::with_capacity(count),
            fluid_wall_temperature: Vec::with_capacity(count),
            fluid_absolute_pressure: Vec::with_capacity(count),
            traction_on_solid: Vec::with_capacity(count),
            solid_nodal_load: vec![[0.0; 3]; self.nn],
            fluid_wall_reaction: Vec::with_capacity(count),
            solid_outward_power: vec![0.0; self.nn],
            fluid_outward_power: Vec::with_capacity(count),
            maximum_temperature_interface_residual: 0.0,
            heat_balance: 0.0,
            force_balance: [0.0; 3],
        };
        let mut jump_max = 0.0_f64;
        for e in 0..count {
            let values: Vec<f64> = (0..WIDTH)
                .map(|j| {
                    let c = self.columns[e * WIDTH + j];
                    usize::try_from(c).map_or(current[e * WIDTH + j], |i| z[i])
                })
                .collect();
            let design: Vec<f64> = self.maps[e * 4..(e + 1) * 4].iter().map(|i| x[*i]).collect();
            let p = k.physical(e, &values, &design);
            let w = k.trace_weights[e];
            for (i, node) in self.nodes[e].iter().enumerate() {
                out.solid_outward_power[*node] += p.area * w[i] * p.q;
                for a in 0..3 {
                    out.solid_nodal_load[*node][a] += p.area * w[i] * p.traction[a];
                }
            }
            out.fluid_outward_power.push(-p.area * p.q);
            out.fluid_wall_reaction.push(p.traction.map(|t| -p.area * t));
            let ke = self.fluid.law.properties(p.fluid_temperature).1 * self.fluid.law.fraction(design[3]);
            let jump = p.solid_temperature
                - p.fluid_temperature
                - self.contact * p.q
                - 0.5 * design[self.axis] * 1e-3 / ke * p.q;
            jump_max = jump_max.max(jump.abs());
            out.heat_flux.push(p.q);
            out.area.push(p.area);
            out.solid_trace_temperature.push(p.solid_temperature);
            out.fluid_cell_temperature.push(p.fluid_temperature);
            out.fluid_wall_temperature.push(p.fluid_temperature + p.q * (p.resistance - self.contact));
            out.fluid_absolute_pressure.push(p.pressure);
            out.traction_on_solid.push(p.traction);
        }
        out.maximum_temperature_interface_residual = jump_max;
        out.heat_balance =
            out.solid_outward_power.iter().sum::<f64>() + out.fluid_outward_power.iter().sum::<f64>();
        for a in 0..3 {
            out.force_balance[a] = out.solid_nodal_load.iter().map(|l| l[a]).sum::<f64>()
                + out.fluid_wall_reaction.iter().map(|l| l[a]).sum::<f64>();
        }
        Ok(out)
    }

    #[must_use]
    pub fn report(&self) -> Value {
        self.group.report()
    }
}

impl HistoryInterface for ConjugateInterface {
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        self.group.residual(n, z, old, x)
    }
    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.group.jacobian(kind, n, z, old, x)?))
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
        Some(self.group.current_action(n, z, old, x, v, transpose))
    }
}
