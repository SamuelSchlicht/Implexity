// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





use std::sync::Arc;

use serde_json::{Value, json};

use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_physics_solid::solid_history::SolidKernel;
use implexity_solve::local_assembly::{Kind, LocalResidual};

use crate::incompressible_transport::{FluidKernel, NodalFluidLaw};
use crate::local_group::{GroupOps, StepKernel, group, sum_jacobians};

pub const PROFILE: &str = "nodal_dual_upwind_v1";
pub const SMOOTH_PROFILE: &str = "nodal_dual_smooth_upwind_v1";
pub const PROFILES: [&str; 2] = [PROFILE, SMOOTH_PROFILE];
pub const CAPABILITY: &str = "cartesian_mac_nodal_dual_upwind_v1";

pub fn upwind_split<S: Scalar>(u: S, smoothing: Option<S>) -> (S, S) {
    match smoothing {
        None => (u.max_f64(0.0), u.min_f64(0.0)),
        Some(delta) => {
            let magnitude = (u * u + delta * delta).sqrt();
            ((u + magnitude) * 0.5, (u - magnitude) * 0.5)
        }
    }
}

struct Shared {
    law: NodalFluidLaw,
    t0: f64,
    ts: f64,
    us: f64,
    scale: f64,
    smoothing: Option<f64>,
    fixed_t: Vec<Vec<f64>>,
}

impl Shared {
    fn face_smoothing<S: Scalar>(&self, obstruction: S) -> Option<S> {
        self.smoothing.map(|d| self.law.law.fraction(obstruction) * d)
    }
    fn fill_nodes(&self, n: usize, nodes: &[usize], width: usize, current: &mut [f64], previous: &mut [f64]) {
        let k = nodes.len() / (current.len() / width).max(1);
        let count = current.len() / width;
        for e in 0..count {
            for j in 0..k {
                let node = nodes[e * k + j];
                current[e * width + j] = (self.fixed_t[n][node] - self.t0) / self.ts;
                previous[e * width + j] = (self.fixed_t[n.saturating_sub(1)][node] - self.t0) / self.ts;
            }
        }
    }
}

struct EdgeKernel {
    s: Arc<Shared>,
    axes: Vec<usize>,
    nodes: Vec<usize>,
}

impl LocalResidual for EdgeKernel {
    fn residual<S: Scalar>(&self, item: usize, z: &[S], _old: &[S], x: &[S], out: &mut [S]) {
        let s = &self.s;
        let axis = self.axes[item];
        let h = [x[1] * 1e-3, x[2] * 1e-3, x[3] * 1e-3];
        let area = h[0] * h[1] * h[2] / (h[axis] * 4.0);
        let t = [z[0] * s.ts + s.t0, z[1] * s.ts + s.t0];
        let u = (z[2] + z[3]) * s.us / 2.0;
        let law = &s.law.law;
        let k = law.fraction(x[0]) * law.properties((t[0] + t[1]) / 2.0).1;
        let hh = [law.enthalpy(t[0]), law.enthalpy(t[1])];
        let (up, um) = upwind_split(u, s.face_smoothing(x[0]));
        let flux = k * area / h[axis] * (t[0] - t[1]) + area * s.law.density * (up * hh[0] + um * hh[1]);
        out[0] = flux / s.scale;
        out[1] = -flux / s.scale;
    }
}

impl StepKernel for EdgeKernel {
    fn prescribed_state(&self, n: usize, current: &mut [f64], previous: &mut [f64]) {
        self.s.fill_nodes(n, &self.nodes, 4, current, previous);
    }
}

struct BoundaryKernel {
    s: Arc<Shared>,
    axis: usize,
    sign: f64,
    open: bool,
    temperature: bool,
    incoming: Vec<f64>,
    tb: Vec<f64>,
    nodes: Vec<usize>,
}

impl BoundaryKernel {
    fn power<S: Scalar>(&self, z: &[S], x: &[S]) -> S {
        let s = &self.s;
        let law = &s.law.law;
        let h = [x[1] * 1e-3, x[2] * 1e-3, x[3] * 1e-3];
        let area = h[0] * h[1] * h[2] / (h[self.axis] * 4.0);
        let t = z[0] * s.ts + s.t0;
        let un = z[1] * (self.sign * s.us);
        let mut power = S::zero();
        if self.open {
            let (up, um) = upwind_split(un, s.face_smoothing(x[0]));
            power = area * s.law.density * (up * law.enthalpy(t) + um * law.enthalpy(z[2]));
        }
        if self.temperature {
            power += law.fraction(x[0]) * 2.0 * law.properties(t).1 * area / h[self.axis] * (t - z[3]);
        }
        power
    }
}

impl LocalResidual for BoundaryKernel {
    fn residual<S: Scalar>(&self, _item: usize, z: &[S], _old: &[S], x: &[S], out: &mut [S]) {
        out[0] = self.power(z, x) / self.s.scale;
    }
}

impl StepKernel for BoundaryKernel {
    fn data_width(&self) -> usize {
        2
    }
    fn step_data(&self, n: usize, out: &mut [f64]) {
        for e in 0..self.nodes.len() {
            out[2 * e] = self.incoming[n];
            out[2 * e + 1] = self.tb[n];
        }
    }
    fn prescribed_state(&self, n: usize, current: &mut [f64], previous: &mut [f64]) {
        self.s.fill_nodes(n, &self.nodes, 2, current, previous);
    }
}

struct NodalGroup {
    ops: Box<dyn GroupOps>,
    boundary: Option<String>,
}

pub struct CartesianNodalTransport {
    pub profile: String,
    pub smoothing_velocity_m_s: Option<f64>,
    shared: Arc<Shared>,
    groups: Vec<NodalGroup>,
    state_size: usize,
    design_size: usize,
}

impl std::fmt::Debug for CartesianNodalTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CartesianNodalTransport").field("profile", &self.profile).finish_non_exhaustive()
    }
}

impl CartesianNodalTransport {

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn new(
        state_size: usize,
        design_size: usize,
        solid_start: usize,
        fluid_start: usize,
        solid: &SolidKernel,
        fluid: &FluidKernel,
        profile: &str,
        smoothing_velocity_m_s: Option<f64>,
        temperature_start: Option<usize>,
    ) -> CaeResult<Self> {
        if !PROFILES.contains(&profile) {
            return Err(CaeError::contract("unsupported nodal transport profile"));
        }
        if (profile == SMOOTH_PROFILE) != smoothing_velocity_m_s.is_some() {
            return Err(CaeError::contract(
                "the smooth nodal upwind profile requires exactly one positive smoothing velocity",
            ));
        }
        if let Some(d) = smoothing_velocity_m_s
            && (!d.is_finite() || d <= 0.0)
        {
            return Err(CaeError::contract("nodal upwind smoothing velocity must be finite and positive"));
        }
        if fluid.nodal_transport_capability() != Some(CAPABILITY) {
            return Err(CaeError::contract("fluid has not declared Cartesian nodal transport capability"));
        }
        if solid.grid != fluid.grid {
            return Err(CaeError::contract("nodal transport requires coincident Cartesian grids"));
        }
        let law = fluid.nodal_constitutive_law()?;
        let m = &solid.model;
        let shared = Arc::new(Shared {
            law,
            t0: m.t0,
            ts: m.ts,
            us: fluid.us,
            scale: m.ks * m.ts * m.ls,
            smoothing: smoothing_velocity_m_s,
            fixed_t: solid.fixed_t.clone(),
        });
        let rows_start = temperature_start.unwrap_or(solid_start + solid.thermal_row_slice().start);
        let mut tmap = vec![-1i64; solid.nn];
        for (k, node) in solid.free_t.iter().enumerate() {
            tmap[*node] = i64::try_from(rows_start + k).unwrap_or(-1);
        }
        let grid = solid.grid;
        let node_shape = [grid[0] + 1, grid[1] + 1, grid[2] + 1];
        let node_id = |q: [usize; 3]| (q[0] * node_shape[1] + q[1]) * node_shape[2] + q[2];
        let velocity_id = |axis: usize, index: [usize; 3]| -> i64 {
            let local = fluid.map(axis, index);
            if local < 0 { -1 } else { i64::try_from(fluid_start).unwrap_or(0) + local }
        };
        let i = |v: usize| i64::try_from(v).unwrap_or(-1);
        let nc = solid.nc;
        let batch = solid.p["assembly"]["batch_size"]
            .as_u64()
            .min(fluid.p["assembly"]["batch_size"].as_u64())
            .and_then(|b| usize::try_from(b).ok())
            .unwrap_or(64)
            .max(1);
        let mut groups = Vec::new();

        let (mut rows, mut current, mut design, mut axes, mut nodes) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for cell in crate::incompressible_transport::kernel_ndindex(grid) {
            let ci = fluid.cell_index(cell);
            for axis in 0..3 {
                let transverse: Vec<usize> = (0..3).filter(|a| *a != axis).collect();
                let mut far = cell;
                far[axis] += 1;
                let lo_v = velocity_id(axis, cell);
                let hi_v = velocity_id(axis, far);
                for bits in [[0usize, 0usize], [0, 1], [1, 0], [1, 1]] {
                    let mut lo = cell;
                    for (a, bit) in transverse.iter().zip(bits) {
                        lo[*a] += bit;
                    }
                    let mut hi = lo;
                    hi[axis] += 1;
                    let (ni, nj) = (node_id(lo), node_id(hi));
                    rows.push(vec![tmap[ni], tmap[nj]]);
                    current.push(vec![tmap[ni], tmap[nj], lo_v, hi_v]);
                    design.push(vec![i(ci), i(nc), i(nc + 1), i(nc + 2)]);
                    axes.push(axis);
                    nodes.extend([ni, nj]);
                }
            }
        }
        let edge = EdgeKernel { s: Arc::clone(&shared), axes, nodes };
        if let Some(g) = group(edge, &rows, &current, &design, state_size, design_size, batch)? {
            groups.push(NodalGroup { ops: g, boundary: None });
        }

        for b in &fluid.boundaries {
            if b.thermal == "interface" {
                return Err(CaeError::contract(
                    "fixed fluid thermal interfaces are unsupported for shared nodal transport",
                ));
            }
            let is_open = b.pressure;
            let is_t = b.thermal == "temperature";
            if !(is_open || is_t) {
                continue;
            }
            let (mut rows, mut current, mut design, mut nodes) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let hi_side = b.side == "hi";
            for cell in fluid.face_cells(b.axis, &b.side) {
                let mut face = cell;
                if hi_side {
                    face[b.axis] += 1;
                }
                let vi = velocity_id(b.axis, face);
                let ci = fluid.cell_index(cell);
                let transverse: Vec<usize> = (0..3).filter(|a| *a != b.axis).collect();
                for bits in [[0usize, 0usize], [0, 1], [1, 0], [1, 1]] {
                    let mut node = face;
                    for (a, bit) in transverse.iter().zip(bits) {
                        node[*a] += bit;
                    }
                    let ni = node_id(node);
                    rows.push(vec![tmap[ni]]);
                    current.push(vec![tmap[ni], vi]);
                    design.push(vec![i(ci), i(nc), i(nc + 1), i(nc + 2)]);
                    nodes.push(ni);
                }
            }
            let history = |key: &str| -> Vec<f64> {
                b.raw.get(key).and_then(Value::as_array).map_or_else(
                    || vec![fluid.t0; fluid.nt],
                    |a| a.iter().filter_map(Value::as_f64).collect(),
                )
            };
            let kernel = BoundaryKernel {
                s: Arc::clone(&shared),
                axis: b.axis,
                sign: if hi_side { 1.0 } else { -1.0 },
                open: is_open,
                temperature: is_t,
                incoming: history("incoming_temperature_K"),
                tb: history("temperature_K"),
                nodes,
            };
            if let Some(g) = group(kernel, &rows, &current, &design, state_size, design_size, batch)? {
                groups.push(NodalGroup { ops: g, boundary: Some(format!("{}_{}", b.axis, b.side)) });
            }
        }
        Ok(Self {
            profile: profile.to_string(),
            smoothing_velocity_m_s,
            shared,
            groups,
            state_size,
            design_size,
        })
    }


    pub fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        let mut out = vec![0.0; self.state_size];
        for g in &self.groups {
            for (o, v) in out.iter_mut().zip(g.ops.residual(n, z, old, x)?) {
                *o += v;
            }
        }
        Ok(out)
    }


    pub fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<CsrMatrix> {
        let cols = if kind == Kind::Design { self.design_size } else { self.state_size };
        sum_jacobians(self.groups.iter().map(|g| g.ops.as_ref()), kind, n, z, old, x, (self.state_size, cols))
    }


    #[allow(clippy::too_many_arguments)]
    pub fn current_action(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        v: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        let mut out = vec![0.0; self.state_size];
        for g in &self.groups {
            for (o, a) in out.iter_mut().zip(g.ops.current_action(n, z, old, x, v, transpose)?) {
                *o += a;
            }
        }
        Ok(out)
    }

    fn power(&self, g: &NodalGroup, n: usize, z: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        Ok(g.ops.local_values(n, z, x)?.into_iter().map(|v| v * self.shared.scale).collect())
    }


    pub fn boundary_energy(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<Vec<(String, f64)>> {
        let mut out = Vec::new();
        for g in &self.groups {
            if let Some(name) = &g.boundary {
                out.push((name.clone(), self.power(g, n, z, x)?.iter().sum()));
            }
        }
        Ok(out)
    }


    pub fn net_outward_power(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<f64> {
        let mut power: f64 = self.residual(n, z, z, x)?.iter().sum::<f64>() * self.shared.scale;
        for g in &self.groups {
            let rows = &g.ops.rows().values;
            if rows.iter().any(|r| *r < 0) {
                let values = self.power(g, n, z, x)?;
                power += rows.iter().zip(&values).filter(|(r, _)| **r < 0).map(|(_, v)| *v).sum::<f64>();
            }
        }
        Ok(power)
    }

    #[must_use]
    pub fn report(&self) -> Value {
        let smooth = self.profile == SMOOTH_PROFILE;
        let mut row = json!({"profile": self.profile,
            "velocity_reconstruction": "RT0_MAC_midcell_normal_velocity",
            "edge_area": "one_quarter_transverse_primal_cell_face",
            "transport": "nodal_two_point_diffusion_and_first_order_upwind_enthalpy",
            "mass_divergence_identity": "nodal_divergence_equals_Q_transpose_cell_divergence",
            "thermal_boundary": "authored_upwind_reservoir_and_optional_halfcell_weak_conduction",
            "derivative": if smooth { "exact_local_current_previous_design_AD; C1_smooth_upwind_split" } else { "exact_local_current_previous_design_AD; symmetric_upwind_subgradient_at_zero" },
            "temperature_clipping": false,
            "numerical_diffusion": if smooth { "first_order_upwind_plus_smoothing_velocity_diffusion; mesh_sensitivity_required" } else { "first_order_upwind; mesh_sensitivity_required" },
            "boundedness_scope": "fixed_positive_coefficients_and_discretely_solenoidal_MAC_flow; not_arbitrary_coupled_nonlinear_physics"});
        if smooth {
            row["upwind_smoothing_velocity_m_s"] = json!(self.smoothing_velocity_m_s);
        }
        row
    }

    #[must_use]
    pub fn group_reports(&self) -> Vec<Value> {
        self.groups.iter().map(|g| g.ops.report()).collect()
    }
}
