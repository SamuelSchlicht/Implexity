// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::sync::Arc;

use ndarray::{Array3, ArrayD, ArrayView3, ArrayViewD, Ix3};
use serde_json::{Map, Value, json};

use implexity_linalg::CsrMatrix;

use crate::diagnostics::{DimensionlessInputs, dimensionless, np_sum};
use crate::error::{CfdError, CfdResult};
use crate::numerics::adjoint::solve_discrete_adjoint;
use crate::numerics::preconditioner::{FactorizationStrategy, SaddlePointLduPreconditioner};
use crate::numerics::solver::{KrylovMethod, SaddlePointKrylovSolver, SaddlePointSolverConfig};
use crate::numerics::system::SaddlePointSystem;
use crate::preflight::run_preflight;
use crate::workspace_contract::{CfdProblem, FACES, face_axis, face_is_min, face_normal, port_response};

pub const RESULT_SCHEMA: &str = "implexity-resolved-stokes-result/1";
pub const MULTI_SENSITIVITY_SCHEMA: &str = "implexity-resolved-stokes-multi-sensitivity/1";
pub const BACKEND_ID: &str = "resolved-staggered-stokes-brinkman-v23-stage2";
pub const QUALIFIED_RESPONSES: [&str; 3] = ["pressure_drop", "volume_flow", "pumping_power"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VelocityDof {
    pub component: usize,
    pub index: [usize; 3],
    pub adjacent_cells: Vec<[usize; 3]>,
}

fn component_shape(cells: [usize; 3], component: usize) -> [usize; 3] {
    let mut s = cells;
    s[component] += 1;
    s
}

fn flat(shape: [usize; 3], idx: [usize; 3]) -> usize {
    (idx[0] * shape[1] + idx[1]) * shape[2] + idx[2]
}

fn unflat(shape: [usize; 3], mut f: usize) -> [usize; 3] {
    let k = f % shape[2];
    f /= shape[2];
    let j = f % shape[1];
    [f / shape[1], j, k]
}

fn face_name(axis: usize, side: usize) -> &'static str {
    FACES[2 * axis + side]
}

fn adjacent_cells(cells: [usize; 3], component: usize, idx: [usize; 3]) -> Vec<[usize; 3]> {
    let n = cells[component];
    let q = idx[component];
    let mut out = Vec::with_capacity(2);
    if q > 0 {
        let mut c = idx;
        c[component] = q - 1;
        out.push(c);
    }
    if q < n {
        let mut c = idx;
        c[component] = q;
        out.push(c);
    }
    out
}

#[must_use]
pub fn ramp(s: f64, q: f64) -> (f64, f64) {
    let den = 1.0 + q * (1.0 - s);
    (s / den, (1.0 + q) / (den * den))
}

#[derive(Debug, Clone)]
pub struct StaggeredAssembly {
    pub problem: CfdProblem,
    pub solid_fraction: Array3<f64>,
    pub continuation: f64,
    pub system: Arc<SaddlePointSystem>,
    pub velocity_dofs: Vec<VelocityDof>,
    pub velocity_maps: [Vec<Option<usize>>; 3],
    pub prescribed: [Vec<Option<f64>>; 3],
    pub full_b: CsrMatrix,
    pub pressure_keep: Vec<usize>,
    pub gauge_cell_flat: Option<usize>,
    pub gauge_value_pa: f64,
    pub alpha_cell: Array3<f64>,
    pub dalpha_ds_cell: Array3<f64>,
}

impl StaggeredAssembly {
    #[must_use]
    pub fn shape(&self) -> [usize; 3] {
        self.problem.domain.cells
    }

    #[must_use]
    pub fn n_cells(&self) -> usize {
        self.problem.domain.n_cells()
    }

    #[must_use]
    pub fn velocity_row(&self, component: usize, idx: [usize; 3]) -> Option<usize> {
        self.velocity_maps[component][flat(component_shape(self.shape(), component), idx)]
    }


    pub fn pressure_full(&self, pressure_reduced: &[f64]) -> CfdResult<Array3<f64>> {
        let n = self.n_cells();
        let mut p = vec![0.0; n];
        if self.pressure_keep.len() == n {
            if pressure_reduced.len() != n {
                return Err(CfdError::Contract("reduced pressure has the wrong size".into()));
            }
            p.copy_from_slice(pressure_reduced);
        } else {
            if pressure_reduced.len() != self.pressure_keep.len() {
                return Err(CfdError::Contract("reduced pressure has the wrong size".into()));
            }
            for (k, v) in self.pressure_keep.iter().zip(pressure_reduced) {
                p[*k] = *v;
            }
            if let Some(g) = self.gauge_cell_flat {
                p[g] = self.gauge_value_pa;
            }
            if self.problem.gauge.mode == "mean_zero" {
                let mean = np_sum(&p) / n as f64;
                let shift = self.gauge_value_pa - mean;
                for v in &mut p {
                    *v += shift;
                }
            }
        }
        let s = self.shape();
        Array3::from_shape_vec((s[0], s[1], s[2]), p).map_err(|e| CfdError::Contract(e.to_string()))
    }


    pub fn face_velocity_arrays(&self, velocity: &[f64]) -> CfdResult<[Array3<f64>; 3]> {
        if velocity.len() != self.velocity_dofs.len() {
            return Err(CfdError::Contract("velocity has the wrong size".into()));
        }
        let cells = self.shape();
        let mut out: Vec<Array3<f64>> = Vec::with_capacity(3);
        for comp in 0..3 {
            let sc = component_shape(cells, comp);
            let mut data = vec![f64::NAN; sc[0] * sc[1] * sc[2]];
            for (f, v) in self.prescribed[comp].iter().enumerate() {
                if let Some(v) = v {
                    data[f] = *v;
                }
            }
            for (f, row) in self.velocity_maps[comp].iter().enumerate() {
                if let Some(row) = row {
                    data[f] = velocity[*row];
                }
            }
            if data.iter().any(|v| v.is_nan()) {
                return Err(CfdError::Contract(
                    "staggered velocity reconstruction left undefined face values".into(),
                ));
            }
            out.push(
                Array3::from_shape_vec((sc[0], sc[1], sc[2]), data)
                    .map_err(|e| CfdError::Contract(e.to_string()))?,
            );
        }
        let w = out.pop().unwrap_or_default();
        let v = out.pop().unwrap_or_default();
        let u = out.pop().unwrap_or_default();
        Ok([u, v, w])
    }


    pub fn divergence(&self, velocity: &[f64]) -> CfdResult<Array3<f64>> {
        let [u, v, w] = self.face_velocity_arrays(velocity)?;
        let [dx, dy, dz] = self.problem.domain.spacing_m();
        let s = self.shape();
        Ok(Array3::from_shape_fn((s[0], s[1], s[2]), |(i, j, k)| {
            (u[[i + 1, j, k]] - u[[i, j, k]]) / dx
                + (v[[i, j + 1, k]] - v[[i, j, k]]) / dy
                + (w[[i, j, k + 1]] - w[[i, j, k]]) / dz
        }))
    }


    pub fn face_outward_flows(&self, velocity: &[f64]) -> CfdResult<Vec<(String, f64)>> {
        let [u, v, w] = self.face_velocity_arrays(velocity)?;
        let [dx, dy, dz] = self.problem.domain.spacing_m();
        let s = self.shape();
        let sum_x = |i: usize| np_sum(&u.index_axis(ndarray::Axis(0), i).iter().copied().collect::<Vec<_>>());
        let sum_y = |j: usize| np_sum(&v.index_axis(ndarray::Axis(1), j).iter().copied().collect::<Vec<_>>());
        let sum_z = |k: usize| np_sum(&w.index_axis(ndarray::Axis(2), k).iter().copied().collect::<Vec<_>>());
        Ok(vec![
            ("x_min".into(), -sum_x(0) * dy * dz),
            ("x_max".into(), sum_x(s[0]) * dy * dz),
            ("y_min".into(), -sum_y(0) * dx * dz),
            ("y_max".into(), sum_y(s[1]) * dx * dz),
            ("z_min".into(), -sum_z(0) * dx * dy),
            ("z_max".into(), sum_z(s[2]) * dx * dy),
        ])
    }

    #[must_use]
    pub fn residual_design_vjp(&self, adjoint: &[f64], state: &[f64]) -> Vec<f64> {
        let s = self.shape();
        let mut grad = vec![0.0; self.n_cells()];
        for (row, dof) in self.velocity_dofs.iter().enumerate() {
            let cells = &dof.adjacent_cells;
            if cells.is_empty() {
                continue;
            }
            let mut coeff = adjoint[row] * state[row] / cells.len() as f64;
            let q = dof.index[dof.component];
            if q == 0 || q == s[dof.component] {
                coeff *= 0.5;
            }
            for c in cells {
                grad[flat(s, *c)] += coeff * self.dalpha_ds_cell[[c[0], c[1], c[2]]];
            }
        }
        grad
    }
}

fn normal_component_value(p: &CfdProblem, face: &str) -> CfdResult<Option<f64>> {
    let bc =
        p.boundary_on(face).ok_or_else(|| CfdError::Qualification(format!("face {face} has no boundary")))?;
    let axis = face_axis(face);
    let n = face_normal(face);
    match bc.kind.as_str() {
        "no_slip" | "symmetry" => Ok(Some(0.0)),
        "velocity" | "moving_wall" => Ok(Some(bc.velocity_m_s.map_or(0.0, |v| v[axis]))),
        "volume_flow" | "mass_flow" => {
            let q_in = bc.inward_volume_flow_m3_s(&p.domain, &p.fluid).unwrap_or(0.0);
            let outward_speed = -q_in / p.domain.face_area_m2(face);
            Ok(Some(outward_speed * n[axis]))
        }
        "pressure" | "traction_outlet" => Ok(None),
        other => Err(CfdError::Qualification(format!(
            "unsupported normal boundary kind {}",
            implexity_core::py_repr::repr_str(other)
        ))),
    }
}

fn tangential_bc_value(p: &CfdProblem, face: &str, component: usize) -> CfdResult<(bool, f64)> {
    let bc =
        p.boundary_on(face).ok_or_else(|| CfdError::Qualification(format!("face {face} has no boundary")))?;
    match bc.kind.as_str() {
        "no_slip" => Ok((true, 0.0)),
        "velocity" | "moving_wall" => Ok((true, bc.velocity_m_s.map_or(0.0, |v| v[component]))),
        "symmetry" | "pressure" | "traction_outlet" | "volume_flow" | "mass_flow" => Ok((false, 0.0)),
        other => Err(CfdError::Qualification(format!(
            "unsupported tangential boundary kind {}",
            implexity_core::py_repr::repr_str(other)
        ))),
    }
}

fn shape_repr(shape: &[usize]) -> String {
    let parts: Vec<String> = shape.iter().map(ToString::to_string).collect();
    if parts.len() == 1 { format!("({},)", parts[0]) } else { format!("({})", parts.join(", ")) }
}

fn checked_topology(p: &CfdProblem, s: &ArrayViewD<'_, f64>) -> CfdResult<Array3<f64>> {
    if s.shape() != p.domain.cells.as_slice() {
        return Err(CfdError::Contract(format!(
            "solid_fraction shape {} differs from CFD cell shape {}",
            shape_repr(s.shape()),
            p.domain.cells_repr()
        )));
    }
    if s.iter().any(|v| !v.is_finite() || *v < 0.0 || *v > 1.0) {
        return Err(CfdError::Contract(
            "model:control/solid_fraction must be finite and lie in [0, 1]".into(),
        ));
    }
    s.to_owned()
        .into_dimensionality::<Ix3>()
        .map(|a| a.as_standard_layout().to_owned())
        .map_err(|e| CfdError::Contract(e.to_string()))
}


#[allow(clippy::too_many_lines)]
pub fn assemble_stokes_brinkman(
    problem: &CfdProblem,
    solid_fraction: &ArrayViewD<'_, f64>,
    continuation: f64,
) -> CfdResult<StaggeredAssembly> {
    let p = problem;
    if p.solver.model != "stokes_brinkman" {
        return Err(CfdError::Qualification(
            "the qualified resolved backend currently accepts only stokes_brinkman; stationary Navier--Stokes requires a converged nonlinear residual/Jacobian path"
                .into(),
        ));
    }
    if p.thermal.enabled {
        return Err(CfdError::Qualification(
            "conjugate heat transfer is not yet qualified in the resolved stage-2 backend".into(),
        ));
    }
    let cfac = continuation;
    if !(cfac > 0.0 && cfac <= 1.0) {
        return Err(CfdError::Qualification("Brinkman continuation factor must lie in (0, 1]".into()));
    }
    let s = checked_topology(p, solid_fraction)?;
    let pre = run_preflight(p, Some(s.view().into_dyn()));
    if !pre.ok {
        return Err(CfdError::Qualification(format!("CFD preflight failed: {}", pre.error_messages("; "))));
    }

    let mu = p.fluid.dynamic_viscosity_pa_s;
    let inv_k_f = 1.0 / p.brinkman.fluid_permeability_m2;
    let inv_k_s = 1.0 / p.brinkman.solid_permeability_m2;
    let q = p.brinkman.ramp_q;
    let alpha_cell = s.mapv(|v| mu * (inv_k_f + cfac * (inv_k_s - inv_k_f) * ramp(v, q).0));
    let dalpha_ds = s.mapv(|v| mu * cfac * (inv_k_s - inv_k_f) * ramp(v, q).1);

    let cells = p.domain.cells;
    let spacing = p.domain.spacing_m();
    let mut maps: [Vec<Option<usize>>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut prescribed: [Vec<Option<f64>>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut dofs: Vec<VelocityDof> = Vec::new();
    for comp in 0..3 {
        let sc = component_shape(cells, comp);
        let n = sc[0] * sc[1] * sc[2];
        maps[comp] = vec![None; n];
        prescribed[comp] = vec![None; n];
        for f in 0..n {
            let idx = unflat(sc, f);
            let qi = idx[comp];
            if qi == 0 || qi == cells[comp] {
                let face = face_name(comp, usize::from(qi != 0));
                if let Some(val) = normal_component_value(p, face)? {
                    prescribed[comp][f] = Some(val);
                    continue;
                }
            }
            maps[comp][f] = Some(dofs.len());
            dofs.push(VelocityDof {
                component: comp,
                index: idx,
                adjacent_cells: adjacent_cells(cells, comp, idx),
            });
        }
    }

    let n_u = dofs.len();
    let (mut rows, mut cols, mut data) = (Vec::new(), Vec::new(), Vec::new());
    let mut f = vec![0.0; n_u];
    let rho = p.fluid.density_kg_m3;
    let gravity = p.domain.gravity_m_s2;
    for (row, dof) in dofs.iter().enumerate() {
        let comp = dof.component;
        let idx = dof.index;
        let boundary_normal = idx[comp] == 0 || idx[comp] == cells[comp];
        let volume_weight = if boundary_normal { 0.5 } else { 1.0 };
        let alphas: Vec<f64> = dof.adjacent_cells.iter().map(|c| alpha_cell[[c[0], c[1], c[2]]]).collect();
        let mean = if alphas.len() == 2 { f64::midpoint(alphas[0], alphas[1]) } else { alphas[0] / 1.0 };
        let mut diag = volume_weight * mean;
        f[row] += volume_weight * rho * gravity[comp];
        let sc = component_shape(cells, comp);
        for axis in 0..3 {
            let h = spacing[axis];
            let mut coeff = volume_weight * mu / (h * h);
            if boundary_normal && axis == comp {
                coeff *= 2.0;
            }
            for side in 0..2 {
                let inside = if side == 0 { idx[axis] >= 1 } else { idx[axis] + 1 < sc[axis] };
                if inside {
                    let mut nb = idx;
                    if side == 0 {
                        nb[axis] -= 1;
                    } else {
                        nb[axis] += 1;
                    }
                    let nf = flat(sc, nb);
                    if let Some(col) = maps[comp][nf] {
                        diag += coeff;
                        rows.push(row);
                        cols.push(col);
                        data.push(-coeff);
                    } else if let Some(val) = prescribed[comp][nf] {
                        diag += coeff;
                        f[row] += coeff * val;
                    } else {
                        return Err(CfdError::Contract(
                            "velocity stencil encountered an unclassified neighbour".into(),
                        ));
                    }
                } else if axis != comp {
                    let face = face_name(axis, side);
                    let (dirichlet, value) = tangential_bc_value(p, face, comp)?;
                    if dirichlet {
                        diag += 2.0 * coeff;
                        f[row] += 2.0 * coeff * value;
                    }
                }
            }
        }
        rows.push(row);
        cols.push(row);
        data.push(diag);
    }
    let a = CsrMatrix::from_triplets(n_u, n_u, &rows, &cols, &data)?;

    let n_cells = cells[0] * cells[1] * cells[2];
    let (mut br, mut bcol, mut bv) = (Vec::new(), Vec::new(), Vec::new());
    let mut g_full = vec![0.0; n_cells];
    for (r, g_r) in g_full.iter_mut().enumerate() {
        let cell = unflat(cells, r);
        for (comp, h) in spacing.iter().enumerate() {
            let sc = component_shape(cells, comp);
            let left = cell;
            let mut right = cell;
            right[comp] += 1;
            for (idx, coeff) in [(left, 1.0 / h), (right, -1.0 / h)] {
                let fi = flat(sc, idx);
                if let Some(col) = maps[comp][fi] {
                    br.push(r);
                    bcol.push(col);
                    bv.push(coeff);
                } else {
                    *g_r -= coeff * prescribed[comp][fi].unwrap_or(0.0);
                }
            }
        }
    }
    let b_full = CsrMatrix::from_triplets(n_cells, n_u, &br, &bcol, &bv)?;

    let enabled: Vec<_> = p.enabled_boundaries().collect();
    let has_pressure_boundary = enabled.iter().any(|b| b.kind == "pressure");
    for bcond in &enabled {
        if bcond.kind != "pressure" && bcond.kind != "traction_outlet" {
            continue;
        }
        let pbc = if bcond.kind == "pressure" { bcond.static_pressure_pa.unwrap_or(0.0) } else { 0.0 };
        let axis = face_axis(&bcond.face);
        let side_min = face_is_min(&bcond.face);
        let qface = if side_min { 0 } else { cells[axis] };
        let sc = component_shape(cells, axis);
        for (fi, slot) in maps[axis].iter().enumerate() {
            let idx = unflat(sc, fi);
            if idx[axis] != qface {
                continue;
            }
            let Some(row) = *slot else { continue };
            let mut c = idx;
            c[axis] = if side_min { 0 } else { cells[axis] - 1 };
            let coeff = b_full.get(flat(cells, c), row);
            f[row] += coeff * pbc;
        }
    }

    let gauge_value = p.gauge.value_pa;
    let (pressure_keep, b, g, gauge_flat) = if has_pressure_boundary {
        ((0..n_cells).collect::<Vec<_>>(), b_full.clone(), g_full.clone(), None)
    } else {
        let gauge_flat = match (p.gauge.mode.as_str(), p.gauge.cell) {
            ("cell", Some(c)) => flat(cells, c),
            _ => 0,
        };
        let keep: Vec<usize> = (0..n_cells).filter(|r| *r != gauge_flat).collect();
        if gauge_value != 0.0 {
            let (ci, vi) = b_full.row(gauge_flat);
            let mut dense = vec![0.0; n_u];
            for (c, v) in ci.iter().zip(vi) {
                dense[*c] = *v;
            }
            for (fr, d) in f.iter_mut().zip(&dense) {
                *fr -= d * gauge_value;
            }
        }
        let (mut rr, mut rc, mut rv) = (Vec::new(), Vec::new(), Vec::new());
        for (out_row, r) in keep.iter().enumerate() {
            let (ci, vi) = b_full.row(*r);
            for (c, v) in ci.iter().zip(vi) {
                rr.push(out_row);
                rc.push(*c);
                rv.push(*v);
            }
        }
        let b = CsrMatrix::from_triplets(keep.len(), n_u, &rr, &rc, &rv)?;
        let g: Vec<f64> = keep.iter().map(|r| g_full[*r]).collect();
        (keep, b, g, Some(gauge_flat))
    };
    let c = if p.solver.pressure_stabilization > 0.0 {
        let n_p = b.nrows();
        Some(CsrMatrix::diagonal_matrix(&vec![p.solver.pressure_stabilization; n_p]))
    } else {
        None
    };
    let mut metadata = Map::new();
    metadata.insert("backend".into(), json!(BACKEND_ID));
    metadata.insert("topology_coordinate".into(), json!("model:control"));
    metadata.insert("cells".into(), json!(cells));
    metadata.insert("continuation".into(), json!(cfac));
    metadata.insert("pressure_boundary".into(), json!(has_pressure_boundary));
    metadata.insert("gauge_cell_flat".into(), json!(gauge_flat));
    let system = SaddlePointSystem::new(a, b, c, Some(&f), Some(&g), metadata)?;
    Ok(StaggeredAssembly {
        problem: p.clone(),
        solid_fraction: s,
        continuation: cfac,
        system: Arc::new(system),
        velocity_dofs: dofs,
        velocity_maps: maps,
        prescribed,
        full_b: b_full,
        pressure_keep,
        gauge_cell_flat: gauge_flat,
        gauge_value_pa: gauge_value,
        alpha_cell,
        dalpha_ds_cell: dalpha_ds,
    })
}

#[derive(Debug)]
pub struct ResolvedStokesResult {
    pub assembly: StaggeredAssembly,
    pub state: Vec<f64>,
    pub velocity: Vec<f64>,
    pub pressure_reduced: Vec<f64>,
    pub u_m_s: Array3<f64>,
    pub v_m_s: Array3<f64>,
    pub w_m_s: Array3<f64>,
    pub pressure_pa: Array3<f64>,
    pub divergence_s_1: Array3<f64>,
    pub face_outward_flows_m3_s: Vec<(String, f64)>,
    pub responses: Vec<(String, f64)>,
    pub diagnostics: Map<String, Value>,
    pub preconditioner: SaddlePointLduPreconditioner,
}

#[must_use]
pub fn array3_to_json(a: &ArrayView3<'_, f64>) -> Value {
    Value::Array(
        a.outer_iter()
            .map(|plane| {
                Value::Array(
                    plane
                        .outer_iter()
                        .map(|line| Value::Array(line.iter().map(|v| json!(v)).collect()))
                        .collect(),
                )
            })
            .collect(),
    )
}

fn set_response(responses: &mut Vec<(String, f64)>, name: &str, value: f64) {
    if let Some(slot) = responses.iter_mut().find(|(n, _)| n == name) {
        slot.1 = value;
    } else {
        responses.push((name.to_string(), value));
    }
}

impl ResolvedStokesResult {
    #[must_use]
    pub fn face_flow(&self, face: &str) -> f64 {
        self.face_outward_flows_m3_s.iter().find(|(f, _)| f == face).map_or(0.0, |(_, q)| *q)
    }

    #[must_use]
    pub fn as_dict(&self, include_state: bool) -> Value {
        let mut out = Map::new();
        out.insert("schema".into(), json!(RESULT_SCHEMA));
        out.insert("model".into(), json!("stokes_brinkman"));
        out.insert(
            "responses".into(),
            Value::Object(self.responses.iter().map(|(k, v)| (k.clone(), json!(v))).collect()),
        );
        out.insert("diagnostics".into(), Value::Object(self.diagnostics.clone()));
        let mut fields = Map::new();
        fields.insert("u_m_s".into(), array3_to_json(&self.u_m_s.view()));
        fields.insert("v_m_s".into(), array3_to_json(&self.v_m_s.view()));
        fields.insert("w_m_s".into(), array3_to_json(&self.w_m_s.view()));
        fields.insert("pressure_Pa".into(), array3_to_json(&self.pressure_pa.view()));
        fields.insert("divergence_s-1".into(), array3_to_json(&self.divergence_s_1.view()));
        out.insert("fields".into(), Value::Object(fields));
        if include_state {
            out.insert("state".into(), json!(self.state));
        }
        Value::Object(out)
    }
}

#[must_use]
pub fn boundary_pressure_weights(shape: [usize; 3], face: &str) -> Vec<f64> {
    let axis = face_axis(face);
    let (near, inner) = if face_is_min(face) { (0, 1) } else { (shape[axis] - 1, shape[axis] - 2) };
    let n = shape[0] * shape[1] * shape[2];
    let area_count = (n / shape[axis]) as f64;
    let mut w = vec![0.0; n];
    for (index, coefficient) in [(near, 1.5), (inner, -0.5)] {
        for (f, slot) in w.iter_mut().enumerate() {
            if unflat(shape, f)[axis] == index {
                *slot = coefficient / area_count;
            }
        }
    }
    w
}

fn face_adjacent_pressure_mean(assembly: &StaggeredAssembly, pressure: &Array3<f64>, face: &str) -> f64 {
    match assembly.problem.boundary_on(face).map(|b| b.kind.as_str()) {
        Some("pressure") => {
            assembly.problem.boundary_on(face).and_then(|b| b.static_pressure_pa).unwrap_or(0.0)
        }
        Some("traction_outlet") => 0.0,
        _ => {
            let w = boundary_pressure_weights(assembly.shape(), face);
            pressure.iter().zip(&w).map(|(p, w)| p * w).sum()
        }
    }
}

fn infer_inlet_outlet(p: &CfdProblem) -> CfdResult<(String, String)> {
    let enabled: Vec<_> = p.enabled_boundaries().collect();
    let prescribed_in: Vec<(String, Option<f64>)> =
        enabled.iter().map(|b| (b.face.clone(), b.inward_volume_flow_m3_s(&p.domain, &p.fluid))).collect();
    let positive: Vec<String> =
        prescribed_in.iter().filter(|(_, q)| q.is_some_and(|q| q > 0.0)).map(|(f, _)| f.clone()).collect();
    let negative: Vec<String> =
        prescribed_in.iter().filter(|(_, q)| q.is_some_and(|q| q < 0.0)).map(|(f, _)| f.clone()).collect();
    let mut ports: Vec<String> = positive.iter().chain(&negative).cloned().collect();
    ports.extend(
        enabled
            .iter()
            .filter(|b| b.kind == "pressure" || b.kind == "traction_outlet")
            .map(|b| b.face.clone()),
    );
    ports.sort();
    ports.dedup();
    if ports.len() > 2 || positive.len() > 1 || negative.len() > 1 {
        return Err(CfdError::Qualification(
            "multi-port response requires explicit inlet_face and outlet_face; automatic selection is ambiguous".into(),
        ));
    }
    if let (Some(pos), Some(neg)) = (positive.first(), negative.first()) {
        return Ok((pos.clone(), neg.clone()));
    }
    let mut pressure: Vec<(String, f64)> = enabled
        .iter()
        .filter(|b| b.kind == "pressure")
        .map(|b| (b.face.clone(), b.static_pressure_pa.unwrap_or(0.0)))
        .collect();
    if pressure.len() >= 2 {
        pressure.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let (first, last) = (&pressure[0], &pressure[pressure.len() - 1]);
        #[allow(clippy::float_cmp)]
        let equal = first.1 == last.1;
        if equal {
            return Err(CfdError::Qualification(
                "equal-pressure ports require explicit inlet_face and outlet_face".into(),
            ));
        }
        return Ok((first.0.clone(), last.0.clone()));
    }
    if let Some(pos) = positive.first() {
        let outlets: Vec<_> =
            enabled.iter().filter(|b| b.kind == "pressure" || b.kind == "traction_outlet").collect();
        if let Some(o) = outlets.first() {
            return Ok((pos.clone(), o.face.clone()));
        }
    }
    Err(CfdError::Qualification(
        "response requires identifiable inlet and outlet faces; specify them on the objective".into(),
    ))
}


pub fn selected_faces(
    p: &CfdProblem,
    objective: Option<&crate::workspace_contract::Objective>,
) -> CfdResult<(String, String)> {
    if let Some(o) = objective {
        if o.inlet_face.is_some() != o.outlet_face.is_some() {
            return Err(CfdError::Qualification(
                "specify both inlet_face and outlet_face, or neither for automatic inference".into(),
            ));
        }
        if let (Some(i), Some(out)) = (&o.inlet_face, &o.outlet_face) {
            if i == out {
                return Err(CfdError::Qualification(
                    "response inlet and outlet must be distinct faces".into(),
                ));
            }
            return Ok((i.clone(), out.clone()));
        }
    }
    infer_inlet_outlet(p)
}

fn pressure_face_state_gradient(assembly: &StaggeredAssembly, face: &str) -> Vec<f64> {
    let sys = &assembly.system;
    let mut grad = vec![0.0; sys.size()];
    if matches!(
        assembly.problem.boundary_on(face).map(|b| b.kind.as_str()),
        Some("pressure" | "traction_outlet")
    ) {
        return grad;
    }
    let w = boundary_pressure_weights(assembly.shape(), face);
    let n_u = sys.n_velocity();
    for (k, cell) in assembly.pressure_keep.iter().enumerate() {
        grad[n_u + k] = w[*cell];
    }
    grad
}

fn flow_state_gradient(assembly: &StaggeredAssembly, face: &str, inward: bool) -> Vec<f64> {
    let mut grad = vec![0.0; assembly.system.size()];
    let axis = face_axis(face);
    let ncomp = face_normal(face)[axis];
    let shape = assembly.shape();
    let others: usize = (0..3).filter(|a| *a != axis).map(|a| shape[a]).product();
    let area_cell = assembly.problem.domain.face_area_m2(face) / others as f64;
    let q = if face_is_min(face) { 0 } else { shape[axis] };
    let sc = component_shape(shape, axis);
    for (fi, row) in assembly.velocity_maps[axis].iter().enumerate() {
        if let Some(row) = row
            && unflat(sc, fi)[axis] == q
        {
            let coeff = ncomp * area_cell;
            grad[*row] += if inward { -coeff } else { coeff };
        }
    }
    grad
}

pub type ResponseEvaluation = (f64, Vec<f64>, Vec<f64>);


pub fn evaluate_port_response(
    result: &ResolvedStokesResult,
    face: &str,
    quantity: &str,
) -> CfdResult<ResponseEvaluation> {
    let assembly = &result.assembly;
    let open = assembly.problem.boundary_on(face).is_some_and(|b| {
        matches!(b.kind.as_str(), "velocity" | "volume_flow" | "mass_flow" | "pressure" | "traction_outlet")
    });
    if !open {
        return Err(CfdError::Qualification("port response requires an enabled open boundary face".into()));
    }
    let factor = match quantity {
        "volume_flow" => 1.0,
        "mass_flow" => assembly.problem.fluid.density_kg_m3,
        "mean_normal_velocity" => 1.0 / assembly.problem.domain.face_area_m2(face),
        other => {
            return Err(CfdError::Qualification(format!(
                "unsupported resolved port quantity {}",
                implexity_core::py_repr::repr_str(other)
            )));
        }
    };
    let grad: Vec<f64> = flow_state_gradient(assembly, face, false).iter().map(|g| g * factor).collect();
    Ok((result.face_flow(face) * factor, grad, vec![0.0; assembly.n_cells()]))
}


pub fn evaluate_response(
    result: &ResolvedStokesResult,
    response: Option<&str>,
) -> CfdResult<ResponseEvaluation> {
    let p = &result.assembly.problem;
    let response = match response {
        Some(r) => r.to_string(),
        None => p.objectives.first().map(|o| o.response.clone()).ok_or_else(|| {
            CfdError::Qualification("no default response is declared; request a response explicitly".into())
        })?,
    };
    if let Some((face, quantity)) = port_response(&response) {
        return evaluate_port_response(result, face, quantity);
    }
    let matches: Vec<_> = p.objectives.iter().filter(|o| o.response == response).collect();
    let mut pairs: Vec<(Option<String>, Option<String>)> = Vec::new();
    for o in &matches {
        let pair = (o.inlet_face.clone(), o.outlet_face.clone());
        if !pairs.contains(&pair) {
            pairs.push(pair);
        }
    }
    if pairs.len() > 1 {
        return Err(CfdError::Qualification(format!(
            "response {} has conflicting face selections; use one face pair per named response",
            implexity_core::py_repr::repr_str(&response)
        )));
    }
    let obj = matches.first().copied();
    if !QUALIFIED_RESPONSES.contains(&response.as_str()) {
        return Err(CfdError::Qualification(format!(
            "stage-2 resolved adjoint currently qualifies pressure_drop, volume_flow, and pumping_power; got {}",
            implexity_core::py_repr::repr_str(&response)
        )));
    }
    let (inlet, outlet) = selected_faces(p, obj)?;
    let a = &result.assembly;
    let dp = face_adjacent_pressure_mean(a, &result.pressure_pa, &inlet)
        - face_adjacent_pressure_mean(a, &result.pressure_pa, &outlet);
    let gi = pressure_face_state_gradient(a, &inlet);
    let go = pressure_face_state_gradient(a, &outlet);
    let d_dp: Vec<f64> = gi.iter().zip(&go).map(|(x, y)| x - y).collect();
    let q_in = -result.face_flow(&inlet);
    let d_q = flow_state_gradient(a, &inlet, true);
    let zeros = vec![0.0; a.n_cells()];
    match response.as_str() {
        "pressure_drop" => Ok((dp, d_dp, zeros)),
        "volume_flow" => Ok((q_in, d_q, zeros)),
        _ => {
            let grad = d_dp.iter().zip(&d_q).map(|(a, b)| q_in * a + dp * b).collect();
            Ok((dp * q_in, grad, zeros))
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResponseSensitivity {
    pub value: f64,
    pub gradient: Array3<f64>,
    pub adjoint_relative_residual: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MultiSensitivity {
    pub responses: Vec<(String, ResponseSensitivity)>,
    pub diagnostics: Map<String, Value>,
}

impl MultiSensitivity {
    #[must_use]
    pub fn as_dict(&self) -> Value {
        let mut responses = Map::new();
        for (name, item) in &self.responses {
            responses.insert(
                name.clone(),
                json!({
                    "value": item.value,
                    "gradient": array3_to_json(&item.gradient.view()),
                    "adjoint_relative_residual": item.adjoint_relative_residual,
                }),
            );
        }
        json!({
            "schema": MULTI_SENSITIVITY_SCHEMA,
            "responses": Value::Object(responses),
            "diagnostics": Value::Object(self.diagnostics.clone()),
            "sensitivity_contract": {
                "topology_parameter": "model:control",
                "primal_state_reused": true,
                "residual_jacobian": "exact assembled primal Jacobian",
                "preconditioner_reused": true,
            },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedStokesBrinkmanBackend {
    pub factorization: FactorizationStrategy,
}

impl Default for ResolvedStokesBrinkmanBackend {
    fn default() -> Self {
        Self { factorization: FactorizationStrategy::Ilu }
    }
}

fn to_cells(shape: [usize; 3], v: Vec<f64>) -> CfdResult<Array3<f64>> {
    Array3::from_shape_vec((shape[0], shape[1], shape[2]), v).map_err(|e| CfdError::Contract(e.to_string()))
}

impl ResolvedStokesBrinkmanBackend {

    pub fn new(factorization: &str) -> CfdResult<Self> {
        let factorization = FactorizationStrategy::parse(factorization)
            .map_err(|_| CfdError::Input("factorization must be direct, ilu, or jacobi".into()))?;
        Ok(Self { factorization })
    }


    pub fn solver(&self, problem: &CfdProblem) -> CfdResult<SaddlePointKrylovSolver> {
        let lin = problem.solver.linear_tolerance;
        let maxlin = problem.solver.maximum_linear_iterations;
        SaddlePointKrylovSolver::new(SaddlePointSolverConfig {
            method: KrylovMethod::Gmres,
            relative_tolerance: lin,
            absolute_tolerance: (lin * 1.0e-2).min(1.0e-13),
            restart: maxlin.clamp(20, 100),
            maximum_iterations: maxlin,
            velocity_preconditioner: self.factorization,
            schur_preconditioner: self.factorization,
            velocity_drop_tolerance: 1.0e-5,
            schur_drop_tolerance: 1.0e-5,
            velocity_fill_factor: 20.0,
            schur_fill_factor: 20.0,
            schur_relative_regularization: 1.0e-12,
            require_convergence: true,
        })
    }


    pub fn resolve(
        &self,
        problem: &CfdProblem,
        solid_fraction: &ArrayViewD<'_, f64>,
    ) -> CfdResult<ResolvedStokesResult> {
        let assembly = assemble_stokes_brinkman(problem, solid_fraction, 1.0)?;
        let solver = self.solver(problem)?;
        let prepared = solver.prepare(&assembly.system)?;
        let primal = solver.solve(&assembly.system, None, false, None, Some(&prepared))?;
        let [u, v, w] = assembly.face_velocity_arrays(&primal.velocity)?;
        let pressure = assembly.pressure_full(&primal.pressure)?;
        let div = assembly.divergence(&primal.velocity)?;
        let flows = assembly.face_outward_flows(&primal.velocity)?;
        let throughput = (0.5 * flows.iter().fold(0.0, |acc, (_, q)| acc + q.abs())).max(f64::MIN_POSITIVE);
        let mass_rel = flows.iter().fold(0.0, |acc, (_, q)| acc + q).abs() / throughput;
        let amax = |a: &Array3<f64>| a.iter().fold(f64::NEG_INFINITY, |m, x| m.max(x.abs()));
        let max_speed = amax(&u).max(amax(&v)).max(amax(&w));
        let spacing = problem.domain.spacing_m();
        let dims = dimensionless(&DimensionlessInputs {
            rho_kg_m3: problem.fluid.density_kg_m3,
            mu_pa_s: problem.fluid.dynamic_viscosity_pa_s,
            velocity_m_s: max_speed.max(f64::MIN_POSITIVE),
            length_m: problem.domain.reference_length_m,
            cell_m: spacing[0].min(spacing[1]).min(spacing[2]),
            k_solid_m2: problem.brinkman.solid_permeability_m2,
            k_fluid_m2: problem.brinkman.fluid_permeability_m2,
            cp_j_kg_k: None,
            k_w_m_k: None,
        });
        let div_abs: Vec<f64> = div.iter().map(|x| x.abs()).collect();
        let mut diagnostics = Map::new();
        diagnostics.insert("converged".into(), json!(primal.converged));
        diagnostics.insert("linear_relative_residual".into(), json!(primal.relative_residual));
        diagnostics.insert("linear_residual_norm".into(), json!(primal.residual_norm));
        diagnostics.insert("continuity_residual_norm".into(), json!(primal.continuity_residual_norm));
        diagnostics.insert(
            "divergence_linf_s-1".into(),
            json!(div_abs.iter().fold(f64::NEG_INFINITY, |m, x| m.max(*x))),
        );
        diagnostics.insert("divergence_mean_abs_s-1".into(), json!(np_sum(&div_abs) / div_abs.len() as f64));
        diagnostics.insert("mass_balance_relative".into(), json!(mass_rel));
        diagnostics.insert(
            "face_outward_flows_m3_s".into(),
            Value::Object(flows.iter().map(|(k, v)| (k.clone(), json!(v))).collect()),
        );
        diagnostics.insert("maximum_velocity_m_s".into(), json!(max_speed));
        diagnostics
            .insert("pressure_min_Pa".into(), json!(pressure.iter().fold(f64::INFINITY, |m, x| m.min(*x))));
        diagnostics.insert(
            "pressure_max_Pa".into(),
            json!(pressure.iter().fold(f64::NEG_INFINITY, |m, x| m.max(*x))),
        );
        diagnostics.insert("adjoint_relative_residual".into(), Value::Null);
        diagnostics.insert("backend".into(), json!(BACKEND_ID));
        diagnostics.extend(dims);
        let mut result = ResolvedStokesResult {
            assembly,
            velocity: primal.velocity.clone(),
            pressure_reduced: primal.pressure.clone(),
            state: primal.state,
            u_m_s: u,
            v_m_s: v,
            w_m_s: w,
            pressure_pa: pressure,
            divergence_s_1: div,
            face_outward_flows_m3_s: flows,
            responses: Vec::new(),
            diagnostics,
            preconditioner: prepared,
        };
        let mut responses = Vec::new();
        for objective in &problem.objectives {
            let is_port = port_response(&objective.response).is_some();
            if QUALIFIED_RESPONSES.contains(&objective.response.as_str()) || is_port {
                match evaluate_response(&result, Some(&objective.response)) {
                    Ok((value, _, _)) => set_response(&mut responses, &objective.response, value),
                    Err(CfdError::Qualification(m)) if !is_port => {
                        let _ = m;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        result.responses = responses;
        Ok(result)
    }


    pub fn solve(
        &self,
        problem: &CfdProblem,
        solid_fraction: Option<ArrayViewD<'_, f64>>,
        responses: &[String],
    ) -> CfdResult<ResolvedStokesResult> {
        let Some(s) = solid_fraction else {
            return Err(CfdError::Qualification(
                "resolved topology CFD requires the model:control/solid_fraction field".into(),
            ));
        };
        let mut resolved = self.resolve(problem, &s)?;
        for response in responses {
            let (value, _, _) = evaluate_response(&resolved, Some(response))?;
            set_response(&mut resolved.responses, response, value);
        }
        Ok(resolved)
    }


    pub fn solve_and_adjoint(
        &self,
        problem: &CfdProblem,
        solid_fraction: Option<ArrayViewD<'_, f64>>,
        response: Option<&str>,
    ) -> CfdResult<Value> {
        let Some(s) = solid_fraction else {
            return Err(CfdError::Qualification(
                "resolved topology CFD requires the model:control/solid_fraction field".into(),
            ));
        };
        let mut resolved = self.resolve(problem, &s)?;
        let (value, dj_dw, dj_ds) = evaluate_response(&resolved, response)?;
        let solver = self.solver(problem)?;
        let a = &resolved.assembly;
        let vjp = |lam: &[f64], state: &[f64]| Ok(a.residual_design_vjp(lam, state));
        let adj = solve_discrete_adjoint(
            &a.system,
            &resolved.state,
            &dj_dw,
            &vjp,
            &solver,
            Some(&dj_ds),
            Some(&resolved.preconditioner),
        )?;
        let shape = a.shape();
        resolved
            .diagnostics
            .insert("adjoint_relative_residual".into(), json!(adj.adjoint_solve.relative_residual));
        let gradient = to_cells(shape, adj.total_design_gradient)?;
        let mut out = resolved.as_dict(true);
        if let Value::Object(m) = &mut out {
            let name = match response {
                Some(r) => r.to_string(),
                None => problem.objectives.first().map(|o| o.response.clone()).unwrap_or_default(),
            };
            m.insert("response".into(), json!(name));
            m.insert("value".into(), json!(value));
            m.insert("gradient".into(), array3_to_json(&gradient.view()));
            m.insert("adjoint".into(), json!(adj.adjoint));
            m.insert(
                "sensitivity_contract".into(),
                json!({
                    "topology_parameter": "model:control",
                    "residual_jacobian": "exact assembled primal Jacobian",
                    "transpose": "exact algebraic transpose of retained Jacobian and preconditioner action",
                }),
            );
        }
        Ok(out)
    }


    pub fn solve_and_adjoints(
        &self,
        problem: &CfdProblem,
        solid_fraction: Option<ArrayViewD<'_, f64>>,
        responses: &[String],
    ) -> CfdResult<MultiSensitivity> {
        let Some(s) = solid_fraction else {
            return Err(CfdError::Qualification(
                "resolved topology CFD requires the model:control/solid_fraction field".into(),
            ));
        };
        if responses.is_empty() {
            return Err(CfdError::Qualification("at least one response is required".into()));
        }
        let resolved = self.resolve(problem, &s)?;
        let solver = self.solver(problem)?;
        let a = &resolved.assembly;
        let vjp = |lam: &[f64], state: &[f64]| Ok(a.residual_design_vjp(lam, state));
        let mut items: Vec<(String, ResponseSensitivity)> = Vec::new();
        let mut worst = 0.0_f64;
        for response in responses {
            let (value, dj_dw, dj_ds) = evaluate_response(&resolved, Some(response))?;
            let adj = solve_discrete_adjoint(
                &a.system,
                &resolved.state,
                &dj_dw,
                &vjp,
                &solver,
                Some(&dj_ds),
                Some(&resolved.preconditioner),
            )?;
            let rr = adj.adjoint_solve.relative_residual;
            worst = worst.max(rr);
            let item = ResponseSensitivity {
                value,
                gradient: to_cells(a.shape(), adj.total_design_gradient)?,
                adjoint_relative_residual: rr,
            };
            if let Some(slot) = items.iter_mut().find(|(n, _)| n == response) {
                slot.1 = item;
            } else {
                items.push((response.clone(), item));
            }
        }
        let mut diagnostics = resolved.diagnostics.clone();
        diagnostics.insert("adjoint_relative_residual".into(), json!(worst));
        diagnostics.insert("adjoint_count".into(), json!(items.len()));
        diagnostics.insert("primal_reuse".into(), json!(true));
        Ok(MultiSensitivity { responses: items, diagnostics })
    }
}

#[must_use]
pub fn topology_view(a: &ArrayD<f64>) -> ArrayViewD<'_, f64> {
    a.view()
}
