// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use serde_json::{Value, json};

use implexity_ad::Scalar;
use implexity_core::py_repr::repr_float;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_physics_base::material_domains::interval_status;
use implexity_physics_solid::solid_history::SolidKernel;
use implexity_solve::coupled_history::HistoryInterface;
use implexity_solve::local_assembly::{Kind, LocalResidual};
use implexity_solve::matrix::Jacobian;

use crate::incompressible_transport::{FluidKernel, FluidLaw, GroupSet};
use crate::local_group::{Group, GroupOps, StepKernel, typed_group};

struct CaloricKernel {
    law: FluidLaw,
    rho: f64,
    floor: f64,
    t0: f64,
    ts: f64,
    solid_scale: f64,
    weights: Vec<[f64; 8]>,
    nodes: Vec<[usize; 8]>,
    fixed_t: Arc<Vec<Vec<f64>>>,
    times: Vec<f64>,
    source: Vec<f64>,
}

impl LocalResidual for CaloricKernel {
    fn residual<S: Scalar>(&self, item: usize, current: &[S], previous: &[S], x: &[S], out: &mut [S]) {
        let theta = x[0];
        let h = [x[1] * 1e-3, x[2] * 1e-3, x[3] * 1e-3];
        let volume = h[0] * h[1] * h[2];
        let fraction = (-theta + 1.0) * (1.0 - self.floor) + self.floor;
        let dt = current[8];
        let source = current[9];
        for j in 0..8 {
            let tno = previous[j] * self.ts + self.t0;
            let dh = self.law.enthalpy_increment(tno, (current[j] - previous[j]) * self.ts);
            let nodal = fraction * volume * self.weights[item][j] * (dh * self.rho / dt - source);
            out[j] = nodal / self.solid_scale;
        }
    }
}

impl StepKernel for CaloricKernel {
    fn data_width(&self) -> usize {
        2
    }
    fn step_data(&self, n: usize, out: &mut [f64]) {
        let dt = self.times[n] - self.times[n.saturating_sub(1)];
        for e in 0..self.nodes.len() {
            out[2 * e] = dt;
            out[2 * e + 1] = self.source[n];
        }
    }
    fn prescribed_state(&self, n: usize, current: &mut [f64], previous: &mut [f64]) {
        for (e, nodes) in self.nodes.iter().enumerate() {
            for (j, node) in nodes.iter().enumerate() {
                current[e * 8 + j] = (self.fixed_t[n][*node] - self.t0) / self.ts;
                previous[e * 8 + j] = (self.fixed_t[n.saturating_sub(1)][*node] - self.t0) / self.ts;
            }
        }
    }
}

pub struct FluidNodalDualVolume {
    s: Arc<SolidKernel>,
    f: Arc<FluidKernel>,
    pub l: CsrMatrix,
    pub l_free: CsrMatrix,
    pub nodes: Vec<[usize; 8]>,
    pub weights: Vec<[f64; 8]>,
    pub solid_start: usize,
    pub fluid_start: usize,
    pub trows: std::ops::Range<usize>,
    pub solid_scale: f64,
    pub dirichlet_nodes: Vec<usize>,
    pub dirichlet_cell_weight: Vec<f64>,
    pub dirichlet_nodal_mass: Vec<f64>,
    pub transport_profile: String,
    local: Group<CaloricKernel>,
    state_size: usize,
    design_size: usize,
}

impl std::fmt::Debug for FluidNodalDualVolume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FluidNodalDualVolume").field("cells", &self.nodes.len()).finish_non_exhaustive()
    }
}

#[allow(clippy::needless_pass_by_value)]
fn lin(e: implexity_linalg::error::LinalgError) -> CaeError {
    CaeError::contract(e.to_string())
}

impl FluidNodalDualVolume {

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn new(
        state_size: usize,
        design_size: usize,
        solid_slice: (usize, usize),
        fluid_start: usize,
        solid: &Arc<SolidKernel>,
        fluid: &Arc<FluidKernel>,
        dual_map: &CsrMatrix,
        transport_profile: &str,
        temperature_start: Option<usize>,
    ) -> CaeResult<Self> {
        let s = solid;
        let trs = s.thermal_row_slice();
        if trs.end > s.state_size || trs.len() != s.n_t() {
            return Err(CaeError::contract(format!(
                "shared fluid dual volume requires the solid kernel to declare a unit-step thermal_row_slice covering its nT={} free temperature rows inside state_size={}; declared slice({}, {}, None)",
                s.n_t(),
                s.state_size,
                trs.start,
                trs.end
            )));
        }
        let l = dual_map.clone();
        if l.shape() != (s.nc, s.nn) || (0..s.nc).any(|i| l.indptr()[i + 1] - l.indptr()[i] != 8) {
            return Err(CaeError::contract(
                "shared fluid dual volume requires eight T4-lumped vertices per Cartesian cell",
            ));
        }
        let sums = l.matvec(&vec![1.0; s.nn]).map_err(lin)?;
        if l.data().iter().any(|v| *v <= 0.0) || sums.iter().any(|v| (v - 1.0).abs() > 1e-15) {
            return Err(CaeError::contract(
                "shared fluid dual-volume weights must be positive and sum exactly to cell volume",
            ));
        }
        let mut nodes = Vec::with_capacity(s.nc);
        let mut weights = Vec::with_capacity(s.nc);
        for c in 0..s.nc {
            let (idx, val) = l.row(c);
            let cell = [c / (s.grid[1] * s.grid[2]), (c / s.grid[2]) % s.grid[1], c % s.grid[2]];
            for node in idx {
                let q = s.mesh.ijk[*node];
                if (0..3).any(|a| q[a] < cell[a] || q[a] > cell[a] + 1) {
                    return Err(CaeError::contract(
                        "shared fluid dual-volume incidence must remain inside its Cartesian cell",
                    ));
                }
            }
            nodes.push(std::array::from_fn(|j| idx[j]));
            weights.push(std::array::from_fn(|j| val[j]));
        }
        let (sl_start, sl_stop) = solid_slice;
        if sl_stop - sl_start != s.state_size {
            return Err(CaeError::contract(
                "solid residual slice disagrees with the solid kernel state size",
            ));
        }

        let trows = match temperature_start {
            None => sl_start + trs.start..sl_start + trs.end,
            Some(start) => start..start + trs.len(),
        };
        let mut tmap = vec![-1i64; s.nn];
        for (k, node) in s.free_t.iter().enumerate() {
            tmap[*node] = i64::try_from(trows.start + k).unwrap_or(-1);
        }
        let incidence: Vec<Vec<i64>> = nodes.iter().map(|ns| ns.iter().map(|n| tmap[*n]).collect()).collect();
        let i = |v: usize| i64::try_from(v).unwrap_or(-1);
        let design: Vec<Vec<i64>> =
            (0..s.nc).map(|c| vec![i(c), i(s.nc), i(s.nc + 1), i(s.nc + 2)]).collect();
        let m = &s.model;
        let solid_scale = m.ks * m.ts * m.ls;
        let mut free = vec![false; s.nn];
        for node in &s.free_t {
            free[*node] = true;
        }
        let free_col: Vec<i64> = {
            let mut col = vec![-1i64; s.nn];
            for (k, node) in s.free_t.iter().enumerate() {
                col[*node] = i(k);
            }
            col
        };
        let (mut rr, mut cc, mut vv) = (Vec::new(), Vec::new(), Vec::new());
        for c in 0..s.nc {
            let (idx, val) = l.row(c);
            for (node, w) in idx.iter().zip(val) {
                if let Ok(k) = usize::try_from(free_col[*node]) {
                    rr.push(c);
                    cc.push(k);
                    vv.push(*w);
                }
            }
        }
        let l_free = CsrMatrix::from_triplets(s.nc, s.free_t.len(), &rr, &cc, &vv).map_err(lin)?;
        let dirichlet_nodes: Vec<usize> = (0..s.nn).filter(|n| !free[*n]).collect();
        let mut dirichlet_cell_weight = vec![0.0; s.nc];
        let mut position = vec![usize::MAX; s.nn];
        for (k, node) in dirichlet_nodes.iter().enumerate() {
            position[*node] = k;
        }
        let mut dirichlet_nodal_mass = vec![0.0; dirichlet_nodes.len()];
        for c in 0..s.nc {
            let (idx, val) = l.row(c);
            for (node, w) in idx.iter().zip(val) {
                if !free[*node] {
                    dirichlet_cell_weight[c] += w;
                    dirichlet_nodal_mass[position[*node]] += w;
                }
            }
        }
        let batch = s.p["assembly"]["batch_size"]
            .as_u64()
            .min(fluid.p["assembly"]["batch_size"].as_u64())
            .and_then(|b| usize::try_from(b).ok())
            .unwrap_or(64)
            .max(1);
        let times: Vec<f64> = s.times.clone();
        let source: Vec<f64> = fluid.p["volumetric_heat_W_m3"]
            .as_array()
            .map(|q| q.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        let kernel = CaloricKernel {
            law: fluid.law.clone(),
            rho: fluid.rho,
            floor: fluid.law.fraction_floor,
            t0: m.t0,
            ts: m.ts,
            solid_scale,
            weights: weights.clone(),
            nodes: nodes.clone(),
            fixed_t: Arc::new(s.fixed_t.clone()),
            times,
            source,
        };
        let local = typed_group(kernel, &incidence, &incidence, &design, state_size, design_size, batch)?
            .ok_or_else(|| CaeError::contract("shared fluid dual volume requires cells"))?;
        Ok(Self {
            s: Arc::clone(solid),
            f: Arc::clone(fluid),
            l,
            l_free,
            nodes,
            weights,
            solid_start: sl_start,
            fluid_start,
            trows,
            solid_scale,
            dirichlet_nodes,
            dirichlet_cell_weight,
            dirichlet_nodal_mass,
            transport_profile: transport_profile.to_string(),
            local,
            state_size,
            design_size,
        })
    }

    fn fl(&self, v: &[f64]) -> Vec<f64> {
        v[self.fluid_start..self.fluid_start + self.f.state_size].to_vec()
    }

    fn fx(&self, x: &[f64]) -> Vec<f64> {
        x[..self.f.nc + 3].to_vec()
    }

    #[must_use]
    pub fn nodal_temperature(&self, n: usize, z: &[f64]) -> Vec<f64> {
        let mut t = self.s.fixed_t[n].clone();
        for (k, node) in self.s.free_t.iter().enumerate() {
            t[*node] = self.s.model.t0 + self.s.model.ts * z[self.trows.start + k];
        }
        t
    }


    pub fn material_validity(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<Value> {
        let s = &self.s;
        let t = self.nodal_temperature(n, z);
        let physical_cells: Vec<f64> =
            x[..s.nc].iter().map(|v| if 1.0 - v > 0.0 { 1.0 } else { 0.0 }).collect();
        let incident_mass = self.l.matvec_transpose(&physical_cells).map_err(lin)?;
        let incident: Vec<bool> = incident_mass.iter().map(|v| *v > 0.0).collect();
        let phase_aware = self.f.inactive_phase_numerical_material.is_some();
        let checked: Vec<bool> = if phase_aware { incident.clone() } else { vec![true; s.nn] };
        let card = &self.f.card;
        let (lower, upper) = (card.lower, card.upper);
        let domain = interval_status(&t, (card.t_min, card.t_max), (lower, upper), Some(&checked))?;
        let finite: Vec<bool> = t.iter().map(|v| v.is_finite()).collect();
        let violations: Vec<bool> = (0..s.nn)
            .map(|i| checked[i] && (!finite[i] || !(t[i] >= card.t_min && t[i] <= card.t_max)))
            .collect();
        let checked_indices: Vec<usize> = (0..s.nn).filter(|i| checked[*i]).collect();
        let checked_finite = checked_indices.iter().all(|i| t[*i].is_finite());
        let pick = |minimum: bool| -> Option<usize> {
            if checked_indices.is_empty() || !checked_finite {
                return None;
            }
            let mut best = checked_indices[0];
            for i in &checked_indices {
                if (minimum && t[*i] < t[best]) || (!minimum && t[*i] > t[best]) {
                    best = *i;
                }
            }
            Some(best)
        };
        let (imin, imax) = (pick(true), pick(false));
        let all_min = t
            .iter()
            .copied()
            .fold(f64::INFINITY, |a, v| if v.is_nan() || a.is_nan() { f64::NAN } else { a.min(v) });
        let all_max = t
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, |a, v| if v.is_nan() || a.is_nan() { f64::NAN } else { a.max(v) });
        Ok(json!({
            "fluid_dual_volume_nodal_temperature_material_interval_screen_passed": finite.iter().all(|f| *f) && !violations.iter().any(|v| *v),
            "fluid_dual_volume_nodal_temperature_evaluation_domain_screen_passed": domain["evaluation_ok"],
            "fluid_dual_volume_material_domain": domain,
            "fluid_dual_volume_evaluation_lower_K": lower,
            "fluid_dual_volume_evaluation_upper_K": upper,
            "fluid_dual_volume_nodal_temperature_screen_semantics": if phase_aware { "every_node_incident_to_a_positive_physical_fluid_cell" } else { "all_dual_volume_nodes_legacy" },
            "fluid_dual_volume_active_nodal_count": incident.iter().filter(|v| **v).count(),
            "fluid_dual_volume_checked_nodal_count": checked.iter().filter(|v| **v).count(),
            "fluid_dual_volume_violation_nodal_count": violations.iter().filter(|v| **v).count(),
            "fluid_dual_volume_nodal_temperature_min_K": all_min,
            "fluid_dual_volume_nodal_temperature_max_K": all_max,
            "fluid_dual_volume_checked_temperature_min_K": imin.map(|i| t[i]),
            "fluid_dual_volume_checked_temperature_max_K": imax.map(|i| t[i]),
            "fluid_dual_volume_checked_temperature_min_node": imin,
            "fluid_dual_volume_checked_temperature_max_node": imax,
            "fluid_dual_volume_material_lower_K": card.t_min,
            "fluid_dual_volume_material_upper_K": card.t_max,
        }))
    }

    pub fn check(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<()> {
        if n >= self.s.nt {
            return Err(CaeError::contract("invalid nodal material-screen time index"));
        }
        let states: Vec<(usize, &[f64])> = if n == 0 { vec![(n, z)] } else { vec![(n, z), (n - 1, old)] };
        for (index, state) in states {
            let v = self.material_validity(index, state, x)?;
            let report_only = self.f.p["applicability_policy"].as_str() == Some("report_only");
            if report_only
                && self.nodal_temperature(index, state).iter().any(|t| !t.is_finite() || *t <= 0.0)
            {
                return Err(CaeError::convergence("shared fluid temperature must be finite and positive"));
            }
            if !report_only
                && v["fluid_dual_volume_nodal_temperature_evaluation_domain_screen_passed"] != json!(true)
            {
                let show = |k: &str| -> String {
                    match &v[k] {
                        Value::Null => "None".into(),
                        Value::Number(n) if n.is_f64() => repr_float(n.as_f64().unwrap_or(f64::NAN)),
                        other => other.to_string(),
                    }
                };
                return Err(CaeError::convergence(format!(
                    "shared fluid dual-volume node leaves declared material evaluation domain; history_step={index}; nodal_temperature_min_K={}; nodal_temperature_max_K={}; checked_temperature_min_K={}; checked_temperature_max_K={}; checked_temperature_min_node={}; checked_temperature_max_node={}; material_lower_K={}; material_upper_K={}; evaluation_lower_K={}; evaluation_upper_K={}; checked_nodes={}; violating_nodes={}",
                    show("fluid_dual_volume_nodal_temperature_min_K"),
                    show("fluid_dual_volume_nodal_temperature_max_K"),
                    show("fluid_dual_volume_checked_temperature_min_K"),
                    show("fluid_dual_volume_checked_temperature_max_K"),
                    show("fluid_dual_volume_checked_temperature_min_node"),
                    show("fluid_dual_volume_checked_temperature_max_node"),
                    show("fluid_dual_volume_material_lower_K"),
                    show("fluid_dual_volume_material_upper_K"),
                    show("fluid_dual_volume_evaluation_lower_K"),
                    show("fluid_dual_volume_evaluation_upper_K"),
                    show("fluid_dual_volume_checked_nodal_count"),
                    show("fluid_dual_volume_violation_nodal_count"),
                )));
            }
        }
        Ok(())
    }

    fn cells(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        let fluid = self.f.residual(GroupSet::Dissipation, n, &self.fl(z), &self.fl(old), &self.fx(x))?;
        let start = self.f.nv + self.f.nc;
        Ok(fluid[start..].iter().map(|v| v * self.f.hs).collect())
    }

    pub fn enthalpy_j(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<f64> {
        let v = self.material_validity(n, z, x)?;
        let report_only = self.f.p["applicability_policy"].as_str() == Some("report_only");
        if report_only && self.nodal_temperature(n, z).iter().any(|t| !t.is_finite() || *t <= 0.0) {
            return Err(CaeError::convergence("shared fluid temperature must be finite and positive"));
        }
        if !report_only
            && v["fluid_dual_volume_nodal_temperature_evaluation_domain_screen_passed"] != json!(true)
        {
            return Err(CaeError::convergence(
                "cannot ledger invalid shared fluid dual-volume nodal enthalpy",
            ));
        }
        let t = self.nodal_temperature(n, z);
        let nc = self.s.nc;
        let fraction: Vec<f64> = x[..nc].iter().map(|th| self.f.law.fraction(*th)).collect();
        let nodal_fraction = self.l.matvec_transpose(&fraction).map_err(lin)?;
        let volume = x[nc] * 1e-3 * (x[nc + 1] * 1e-3) * (x[nc + 2] * 1e-3);
        let dot: f64 = nodal_fraction.iter().zip(&t).map(|(f, tt)| f * self.f.law.enthalpy(*tt)).sum();
        Ok(self.f.rho * volume * dot)
    }


    pub fn dirichlet_absorbed_dissipation_w(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
    ) -> CaeResult<Value> {
        let cells: Vec<f64> = self.cells(n, z, old, x)?.into_iter().map(|v| -v).collect();
        let total: f64 = cells.iter().sum();
        let free: f64 = self.l_free.matvec_transpose(&cells).map_err(lin)?.iter().sum();
        let absorbed: f64 = self.dirichlet_cell_weight.iter().zip(&cells).map(|(w, c)| w * c).sum();
        Ok(json!({
            "fluid_dissipation_delivered_to_free_solid_rows_W": free,
            "dirichlet_absorbed_fluid_caloric_power_W": absorbed,
            "dirichlet_absorbed_fluid_caloric_fraction": if total == 0.0 { 0.0 } else { absorbed / total },
            "fluid_dissipation_dirichlet_split_residual_W": total - free - absorbed,
        }))
    }

    #[must_use]
    pub fn report(&self) -> Value {
        let nodal = crate::nodal_transport::PROFILES.contains(&self.transport_profile.as_str());
        let mut mass = vec![0.0; self.s.nn];
        for (nodes, ws) in self.nodes.iter().zip(&self.weights) {
            for (n, w) in nodes.iter().zip(ws) {
                mass[*n] += w;
            }
        }
        json!({"method": "parity_kuhn_T4_row_sum_lumped_nodal_fluid_volume",
            "cell_temperature_transport_map": if nodal { "coefficient_sampling_Q_v1" } else { "trilinear_cell_average_Q_v1" },
            "moved_terms": ["fluid_caloric_storage", "fluid_volumetric_heat", "fluid_normal_viscous_dissipation", "fluid_Brinkman_dissipation", "fluid_shear_dissipation"],
            "dissipation_evaluation": "native_cell_and_edge_mu_alpha_velocity_quadrature_then_integrated_power_distributed_by_L; no_nodewise_mu_re_evaluation",
            "terms_retained_on_cell_transport_rows": if nodal { json!([]) } else { json!(["fluid_advection", "fluid_conduction"]) },
            "nodal_dual_volume_zero_weight_nodes": mass.iter().filter(|m| **m <= 0.0).count(),
            "dirichlet_solid_thermal_nodes": self.dirichlet_nodes.len(),
            "dirichlet_fluid_anchor_nodes": self.dirichlet_nodal_mass.iter().filter(|m| **m > 0.0).count(),
            "dirichlet_dual_volume_fraction": self.dirichlet_cell_weight.iter().sum::<f64>() / self.s.nc as f64,
            "dirichlet_absorbed_fluid_caloric_power": "per_step_(L_Dirichlet.1)^T_f_fluid_in_coupling_history_rows_as_dirichlet_absorbed_fluid_caloric_power_W",
            "free_row_conservation_identity": "sum_free_solid_rows_equals_sum_fluid_cells_only_when_dirichlet_fluid_anchor_nodes_is_zero",
            "fluid_split_contract": crate::incompressible_transport::shared_temperature_energy_contract(),
            "caloric_nodal_sparse_assembly": self.local.report(),
            "mapped_native_dissipation_sparse_assemblies": self.f.group_reports(GroupSet::Dissipation),
            "dissipation_row_map": "L_free.T times native_cell_thermal_rows with exact physical-power scale conversion",
            "exact_partials": ["current_state", "previous_state", "solid_fraction", "cell_spacing"]})
    }

    #[must_use]
    pub fn template_capacity(&self, kinds: &[Kind]) -> usize {
        self.local.template_capacity(kinds)
    }
}

impl HistoryInterface for FluidNodalDualVolume {
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        self.check(n, z, old, x)?;
        let mut out = self.local.residual(n, z, old, x)?;
        let cells = self.cells(n, z, old, x)?;
        let mapped = self.l_free.matvec_transpose(&cells).map_err(lin)?;
        for (k, v) in mapped.iter().enumerate() {
            out[self.trows.start + k] += v / self.solid_scale;
        }
        Ok(out)
    }

    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        self.check(n, z, old, x)?;
        let local = self.local.jacobian(kind, n, z, old, x)?;
        let partial =
            self.f.jacobian(GroupSet::Dissipation, kind, n, &self.fl(z), &self.fl(old), &self.fx(x))?;
        let start = self.f.nv + self.f.nc;

        let factor = self.f.hs / self.solid_scale;
        let (mut rr, mut cc, mut vv) = (Vec::new(), Vec::new(), Vec::new());
        let lt = self.l_free.transpose();
        for k in 0..lt.nrows() {
            let (cells, w) = lt.row(k);
            for (cell, weight) in cells.iter().zip(w) {
                let (cols, vals) = partial.row(start + cell);
                for (col, val) in cols.iter().zip(vals) {
                    rr.push(self.trows.start + k);
                    cc.push(if kind == Kind::Design { *col } else { self.fluid_start + col });
                    vv.push(weight * val * factor);
                }
            }
        }
        let width = if kind == Kind::Design { self.design_size } else { self.state_size };
        for i in 0..local.nrows() {
            let (cols, vals) = local.row(i);
            for (c, v) in cols.iter().zip(vals) {
                rr.push(i);
                cc.push(*c);
                vv.push(*v);
            }
        }
        let m = CsrMatrix::from_triplets(self.state_size, width, &rr, &cc, &vv).map_err(lin)?;
        Ok(Jacobian::Csr(implexity_solve::matrix::eliminate_zeros(&m)))
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
            self.check(n, z, old, x)?;
            if v.len() != self.state_size || v.iter().any(|a| !a.is_finite()) {
                return Err(CaeError::contract("shared dual-volume current-action operand is invalid"));
            }
            let mut out = self.local.current_action(n, z, old, x, v, transpose)?;
            let (lz, lo, lx) = (self.fl(z), self.fl(old), self.fx(x));
            let start = self.f.nv + self.f.nc;
            if !transpose {
                let fluid =
                    self.f.current_action(GroupSet::Dissipation, n, &lz, &lo, &lx, &self.fl(v), false)?;
                let cells: Vec<f64> = fluid[start..].iter().map(|a| a * self.f.hs).collect();
                let mapped = self.l_free.matvec_transpose(&cells).map_err(lin)?;
                for (k, a) in mapped.iter().enumerate() {
                    out[self.trows.start + k] += a / self.solid_scale;
                }
                return Ok(out);
            }
            let mut fluid_rows = vec![0.0; self.f.state_size];
            let solid_rows = &v[self.trows.clone()];
            let mapped = self.l_free.matvec(solid_rows).map_err(lin)?;
            for (c, a) in mapped.iter().enumerate() {
                fluid_rows[start + c] = a * (self.f.hs / self.solid_scale);
            }
            let fluid = self.f.current_action(GroupSet::Dissipation, n, &lz, &lo, &lx, &fluid_rows, true)?;
            for (k, a) in fluid.iter().enumerate() {
                out[self.fluid_start + k] += a;
            }
            Ok(out)
        })())
    }
}
