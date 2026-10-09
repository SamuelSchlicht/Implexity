// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use implexity_ad::{Dual, Scalar};
use implexity_core::CaeError;
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::local_assembly::{AssemblyOptions, Incidence, Kind, LocalResidualAssembly};
use implexity_solve::matrix::Jacobian;
use implexity_solve::native_history::{
    HistoryOptions, HistoryProblem, HistorySolution, HistorySolveOptions, NativeHistorySystem,
};

use super::COMPONENT_KINDS;
use crate::components::{SolidComponent, selected_component};
use crate::history::MaterialHistoryBinding;
use crate::inelastic::{InelasticLayout, layout_for, state_scales};
use crate::polymer::BoundMaxwell;
use crate::solid_elements::{Exponent, SolidElement, SolidModel};
use crate::solid_exchange::{BoundaryHost, SolidBoundaryHistory};
use crate::structural_inertia::{Dynamics, InertiaElement, KinematicElement};
use crate::util::{contract, convergence, f};

#[derive(Debug, Clone, PartialEq)]
pub struct SolidMesh {
    pub ijk: Vec<[usize; 3]>,
    pub tets: Vec<[usize; 4]>,
    pub owners: Vec<usize>,
    pub gradients: Vec<[[f64; 3]; 4]>,
}

const PERMUTATIONS: [[usize; 3]; 6] = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
];

fn det3(m: [[f64; 3]; 3]) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

fn node_id(v: [usize; 3], shape: [usize; 3]) -> usize {
    (v[0] * shape[1] + v[1]) * shape[2] + v[2]
}

pub fn mesh(grid: [usize; 3]) -> Result<SolidMesh, CaeError> {
    let shape = [grid[0] + 1, grid[1] + 1, grid[2] + 1];
    let mut ijk = Vec::with_capacity(shape.iter().product());
    for i in 0..shape[0] {
        for j in 0..shape[1] {
            for k in 0..shape[2] {
                ijk.push([i, j, k]);
            }
        }
    }
    let mut tets = Vec::new();
    let mut owners = Vec::new();
    for ci in 0..grid[0] {
        for cj in 0..grid[1] {
            for ck in 0..grid[2] {
                let cell = [ci, cj, ck];
                let parity = cell.map(|c| c % 2);
                let anchor: [i64; 3] =
                    std::array::from_fn(|a| i64::try_from(cell[a] + parity[a]).unwrap_or(i64::MAX));
                let direction: [i64; 3] =
                    std::array::from_fn(|a| if parity[a] == 1 { -1 } else { 1 });
                for perm in PERMUTATIONS {
                    let mut v = anchor;
                    let to_u = |v: [i64; 3]| v.map(|x| usize::try_from(x).unwrap_or(0));
                    let mut verts = vec![node_id(to_u(v), shape)];
                    for ax in perm {
                        v[ax] += direction[ax];
                        verts.push(node_id(to_u(v), shape));
                    }
                    let xyz: Vec<[f64; 3]> =
                        verts.iter().map(|n| ijk[*n].map(|x| x as f64)).collect();
                    let m: [[f64; 3]; 3] =
                        std::array::from_fn(|r| std::array::from_fn(|c| xyz[c + 1][r] - xyz[0][r]));
                    if det3(m) < 0.0 {
                        verts.swap(2, 3);
                    }
                    tets.push([verts[0], verts[1], verts[2], verts[3]]);
                    owners.push((ci * grid[1] + cj) * grid[2] + ck);
                }
            }
        }
    }
    let mut gradients = Vec::with_capacity(tets.len());
    for t in &tets {
        let p: Vec<[f64; 3]> = t.iter().map(|n| ijk[*n].map(|x| x as f64)).collect();
        let m: [[f64; 3]; 3] =
            std::array::from_fn(|r| std::array::from_fn(|c| p[r + 1][c] - p[0][c]));
        let det = det3(m);
        if (det - 1.0).abs() > 1e-8 + 1e-5 {
            return contract("parity-Kuhn mesh requires positive unit reference tetrahedra");
        }

        let inv = inverse3(m, det);
        let mut g = [[0.0; 3]; 4];
        for a in 0..3 {
            for i in 1..4 {
                g[i][a] = inv[a][i - 1];
            }
            g[0][a] = -(g[1][a] + g[2][a] + g[3][a]);
        }
        gradients.push(g);
    }
    Ok(SolidMesh {
        ijk,
        tets,
        owners,
        gradients,
    })
}

fn inverse3(m: [[f64; 3]; 3], det: f64) -> [[f64; 3]; 3] {
    let c =
        |r0: usize, c0: usize, r1: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    [
        [
            c(1, 1, 2, 2) / det,
            -c(0, 1, 2, 2) / det,
            c(0, 1, 1, 2) / det,
        ],
        [
            -c(1, 0, 2, 2) / det,
            c(0, 0, 2, 2) / det,
            -c(0, 0, 1, 2) / det,
        ],
        [
            c(1, 0, 2, 1) / det,
            -c(0, 0, 2, 1) / det,
            c(0, 0, 1, 1) / det,
        ],
    ]
}

fn face_nodes_of(mesh: &SolidMesh, grid: [usize; 3], axis: usize, hi: bool) -> Vec<usize> {
    let target = if hi { grid[axis] } else { 0 };
    (0..mesh.ijk.len())
        .filter(|n| mesh.ijk[*n][axis] == target)
        .collect()
}

fn triangle_counts(mesh: &SolidMesh, grid: [usize; 3], axis: usize, hi: bool) -> Vec<f64> {
    let target = if hi { grid[axis] } else { 0 };
    let mut counts = vec![0usize; mesh.ijk.len()];
    for tet in &mesh.tets {
        let on: Vec<usize> = tet
            .iter()
            .copied()
            .filter(|n| mesh.ijk[*n][axis] == target)
            .collect();
        if on.len() == 3 {
            for n in on {
                counts[n] += 1;
            }
        }
    }
    face_nodes_of(mesh, grid, axis, hi)
        .iter()
        .map(|n| counts[*n] as f64)
        .collect()
}

pub struct SolidKernel {
    pub p: Value,
    pub model: Arc<SolidModel>,
    pub grid: [usize; 3],
    pub mesh: SolidMesh,
    pub nc: usize,
    pub nn: usize,
    pub ne: usize,
    pub nt: usize,
    pub times: Vec<f64>,
    pub fixed_u: Vec<Vec<f64>>,
    pub fixed_t: Vec<Vec<f64>>,
    pub free_u: Vec<usize>,
    pub free_t: Vec<usize>,
    pub tmap: Vec<i64>,
    pub umap: Vec<i64>,
    pub internal_size: usize,
    pub internal_stop: usize,
    pub velocity: std::ops::Range<usize>,
    pub acceleration: std::ops::Range<usize>,
    pub solid_state_size: usize,
    pub state_size: usize,
    pub dynamics: Option<Dynamics>,
    pub compliance_window: (f64, f64),
    pub t_min: f64,
    pub t_max: f64,
    pub local: LocalResidualAssembly<SolidElement>,
    pub inertia: Option<LocalResidualAssembly<InertiaElement>>,
    pub kinematics: Option<LocalResidualAssembly<KinematicElement>>,
    pub boundary: SolidBoundaryHistory,
    face_counts: Mutex<std::collections::BTreeMap<(usize, bool), Vec<f64>>>,
    last: Mutex<Option<(Vec<u64>, Arc<HistorySolution>)>>,
}

impl std::fmt::Debug for SolidKernel {
    fn fmt(&self, fm: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fm.debug_struct("SolidKernel")
            .field("grid", &self.grid)
            .field("state_size", &self.state_size)
            .field("nt", &self.nt)
            .finish_non_exhaustive()
    }
}

fn selected_law(p: &Value, key: &str) -> Result<Option<SolidComponent>, CaeError> {
    let v = &p["components"][key];
    if v.is_null() {
        return Ok(None);
    }
    let kind = COMPONENT_KINDS
        .iter()
        .find(|(k, _)| *k == key)
        .map_or("", |(_, k)| *k);
    selected_component(&implexity_core::pyobj::py_str(v), kind).map(Some)
}

fn usize_of(v: &Value) -> usize {
    usize::try_from(v.as_u64().unwrap_or(0)).unwrap_or(0)
}

impl SolidKernel {
    pub fn local_elimination_partition(
        &self,
        size: usize,
    ) -> Result<Option<Arc<implexity_solve::local_condensation::LocalEliminationPartition>>, CaeError>
    {
        if self.internal_size == 0 {
            return Ok(None);
        }
        let start = self.n_t() + self.n_u();
        let groups = (0..self.ne)
            .map(|e| {
                (start + e * self.internal_size..start + (e + 1) * self.internal_size).collect()
            })
            .collect();
        let partition =
            implexity_solve::local_condensation::LocalEliminationPartition::new(size, groups)
                .map_err(|e| CaeError::contract(e.to_string()))?;
        Ok(Some(Arc::new(partition)))
    }

    #[allow(clippy::too_many_lines)]
    pub fn new(p: Value) -> Result<Self, CaeError> {
        let grid: [usize; 3] = std::array::from_fn(|i| usize_of(&p["grid"][i]));
        let nc = grid.iter().product();
        let times = crate::util::times(&p);
        let nt = times.len();
        let mesh = mesh(grid)?;
        let (ne, nn) = (mesh.tets.len(), mesh.ijk.len());
        let Some(SolidComponent::Material(law)) = selected_law(&p, "material")? else {
            return contract("explicit material selection required");
        };
        let plastic = match selected_law(&p, "plasticity")? {
            Some(SolidComponent::Plastic(l)) => Some(l),
            _ => None,
        };
        let creep = match selected_law(&p, "creep")? {
            Some(SolidComponent::Creep(l)) => Some(l),
            _ => None,
        };
        let mut materials = [
            law.validate(&p["materials"][0])?,
            law.validate(&p["materials"][1])?,
        ];
        if creep == Some(crate::inelastic::CreepLaw::StrainHardening) {
            for i in 0..2 {
                materials[i].creep_curve = Some(crate::three_stage_creep::coefficients(
                    &p["creep_parameters"][i],
                )?);
                materials[i].creep_validity = Some(crate::three_stage_creep::validity(
                    &p["creep_parameters"][i],
                )?);
            }
        }
        let numerical = p
            .get("inactive_phase_numerical_material")
            .is_some_and(|v| !v.is_null());
        super::refuse_chaboche_continuation(law, numerical)?;
        let viscoelastic: Option<BoundMaxwell> = crate::polymer::bind_viscoelastic(
            p.get("viscoelasticity").unwrap_or(&Value::Null),
            &p,
        )?;
        let history: Option<MaterialHistoryBinding> =
            crate::history::bind(p.get("material_history").unwrap_or(&Value::Null), &p)?;
        let layout: InelasticLayout = layout_for(
            plastic,
            creep,
            viscoelastic.as_ref().map(BoundMaxwell::size),
            &materials,
        )?;
        let internal_size = layout.material_start() + history.as_ref().map_or(0, |m| m.size);
        let mut fixed_u = vec![vec![0.0; nn * 3]; nt];
        let mut umask = vec![false; nn * 3];
        let t0 = f(&p, "temperature_initial_K");
        let mut fixed_t = vec![vec![t0; nn]; nt];
        let mut tmask = vec![false; nn];
        let face_nodes = |axis: usize, hi: bool| -> Vec<usize> {
            let target = if hi { grid[axis] } else { 0 };
            (0..nn).filter(|n| mesh.ijk[*n][axis] == target).collect()
        };
        for key in ["displacement_bcs", "temperature_bcs"] {
            for bc in p[key].as_array().into_iter().flatten() {
                let face = face_nodes(usize_of(&bc["axis"]), bc["side"] == json!("hi"));
                let vals: Vec<f64> = bc["values"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|v| v.as_f64().unwrap_or(f64::NAN))
                    .collect();
                for node in face {
                    if key == "displacement_bcs" {
                        let c = usize_of(&bc["component"]);
                        let dof = 3 * node + c;
                        #[allow(clippy::float_cmp)]
                        if umask[dof] && (0..nt).any(|n| fixed_u[n][dof] != vals[n]) {
                            return contract("conflicting displacement BCs at a node");
                        }
                        umask[dof] = true;
                        for (n, row) in fixed_u.iter_mut().enumerate() {
                            row[dof] = vals[n];
                        }
                    } else {
                        #[allow(clippy::float_cmp)]
                        if tmask[node] && (0..nt).any(|n| fixed_t[n][node] != vals[n]) {
                            return contract("conflicting temperature BCs at a node");
                        }
                        tmask[node] = true;
                        for (n, row) in fixed_t.iter_mut().enumerate() {
                            row[node] = vals[n];
                        }
                    }
                }
            }
        }
        let free_u: Vec<usize> = (0..nn * 3).filter(|d| !umask[*d]).collect();
        let free_t: Vec<usize> = (0..nn).filter(|n| !tmask[*n]).collect();
        let (n_u, n_t) = (free_u.len(), free_t.len());
        let internal_stop = n_t + n_u + internal_size * ne;
        let dynamics_json = p
            .get("structural_dynamics")
            .filter(|v| !v.is_null())
            .cloned();
        let dynamics = dynamics_json.as_ref().map(Dynamics::from_json);
        let velocity = internal_stop..internal_stop + if dynamics.is_some() { n_u } else { 0 };
        let acceleration = velocity.end..velocity.end + if dynamics.is_some() { n_u } else { 0 };
        let solid_state_size = acceleration.end;
        let finite_reservoirs = p["thermal_reservoirs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| r["kind"] == json!("finite_capacity"))
            .count();
        let state_size = solid_state_size + finite_reservoirs;
        let numerics = &p["numerics"];
        let (es, ss, ts, ls, ks) = (
            f(numerics, "strain_scale"),
            f(numerics, "stress_scale_Pa"),
            f(numerics, "temperature_scale_K"),
            f(numerics, "length_scale_m"),
            f(numerics, "conductivity_scale_W_mK"),
        );
        let scales = state_scales(
            &layout,
            es,
            viscoelastic.as_ref().map(BoundMaxwell::scales).as_deref(),
            history.as_ref().map(|m| m.scales.as_slice()),
        );
        let window = dynamics_json
            .as_ref()
            .and_then(|d| d.get("compliance_window_s"))
            .and_then(Value::as_array)
            .cloned();
        let compliance_window = match window {
            Some(w) => (w[0].as_f64().unwrap_or(0.0), w[1].as_f64().unwrap_or(0.0)),
            None => (times[0], times[nt - 1]),
        };
        let t_min = materials
            .iter()
            .map(|m| m.t_min)
            .fold(f64::NEG_INFINITY, f64::max);
        let t_max = materials
            .iter()
            .map(|m| m.t_max)
            .fold(f64::INFINITY, f64::min);
        if t_min > t_max {
            return contract("material temperature intervals do not overlap");
        }
        let r = &p["regularisation"];
        let forcing_channels = history.as_ref().map_or(0, |m| m.forcing_shape[2]);
        let model = Arc::new(SolidModel {
            t0,
            ts,
            es,
            ss,
            ls,
            ks,
            us: ls * es,
            stiffness_floor: f(r, "stiffness_floor"),
            conductivity_floor: f(r, "conductivity_floor_W_mK"),
            penalty: Exponent::from_json(&r["topology_penalty"]),
            materials,
            law,
            numerical,
            plastic,
            creep,
            viscoelastic,
            history,
            layout,
            scales,
            internal_size,
            forcing_channels,
        });
        let mut tmap = vec![-1_i64; nn];
        for (k, node) in free_t.iter().enumerate() {
            tmap[*node] = i64::try_from(k).unwrap_or(-1);
        }
        let mut umap = vec![-1_i64; nn * 3];
        for (k, dof) in free_u.iter().enumerate() {
            umap[*dof] = i64::try_from(n_t + k).unwrap_or(-1);
        }
        let local_size = model.local_size();
        let width = model.local_width();
        let to_i = |v: usize| i64::try_from(v).unwrap_or(-1);
        let mut rows = Vec::with_capacity(ne * local_size);
        let mut cols = Vec::with_capacity(ne * width);
        let mut xmap = Vec::with_capacity(ne * 5);
        for (e, tet) in mesh.tets.iter().enumerate() {
            let mut row: Vec<i64> = tet.iter().map(|n| tmap[*n]).collect();
            for n in tet {
                for c in 0..3 {
                    row.push(umap[3 * n + c]);
                }
            }
            for k in 0..internal_size {
                row.push(to_i(n_t + n_u + e * internal_size + k));
            }
            rows.extend_from_slice(&row);
            cols.extend_from_slice(&row);
            cols.extend(std::iter::repeat_n(-1, width - local_size));
            let owner = to_i(mesh.owners[e]);
            let nci = to_i(nc);
            xmap.extend_from_slice(&[owner, nci, nci + 1, nci + 2, nci + 3 + owner]);
        }
        let batch = usize_of(&p["assembly"]["batch_size"]);
        let options = |b: usize| AssemblyOptions {
            batch_size: b,
            ..AssemblyOptions::default()
        };
        let design_size = 2 * nc + 3;
        let grad0 = Arc::new(mesh.gradients.clone());
        let element = SolidElement {
            model: Arc::clone(&model),
            grad0: Arc::clone(&grad0),
        };
        let local = LocalResidualAssembly::new(
            element,
            Incidence::new(ne, local_size, rows)?,
            Incidence::new(ne, width, cols.clone())?,
            Incidence::new(ne, width, cols)?,
            Incidence::new(ne, 5, xmap.clone())?,
            state_size,
            design_size,
            options(batch),
        )?;
        let (mut inertia, mut kinematics) = (None, None);
        if let Some(d) = dynamics {
            let mut vmap = vec![-1_i64; nn * 3];
            let mut amap = vec![-1_i64; nn * 3];
            for (k, dof) in free_u.iter().enumerate() {
                vmap[*dof] = to_i(velocity.start + k);
                amap[*dof] = to_i(acceleration.start + k);
            }
            let mut irows = Vec::with_capacity(ne * 12);
            let mut icols = Vec::with_capacity(ne * 40);
            for tet in &mesh.tets {
                let dofs: Vec<usize> = tet
                    .iter()
                    .flat_map(|n| (0..3).map(move |c| 3 * n + c))
                    .collect();
                irows.extend(dofs.iter().map(|d| umap[*d]));
                icols.extend(tet.iter().map(|n| tmap[*n]));
                icols.extend(dofs.iter().map(|d| umap[*d]));
                icols.extend(dofs.iter().map(|d| vmap[*d]));
                icols.extend(dofs.iter().map(|d| amap[*d]));
            }
            inertia = Some(LocalResidualAssembly::new(
                InertiaElement {
                    model: Arc::clone(&model),
                    dynamics: d,
                    grad0: Arc::clone(&grad0),
                },
                Incidence::new(ne, 12, irows)?,
                Incidence::new(ne, 40, icols.clone())?,
                Incidence::new(ne, 40, icols)?,
                Incidence::new(ne, 5, xmap)?,
                state_size,
                design_size,
                options(batch),
            )?);
            let mut krows = Vec::with_capacity(n_u * 2);
            let mut kcols = Vec::with_capacity(n_u * 4);
            for (k, dof) in free_u.iter().enumerate() {
                krows.extend_from_slice(&[vmap[*dof], amap[*dof]]);
                kcols.extend_from_slice(&[to_i(n_t + k), vmap[*dof], amap[*dof], -1]);
            }
            kinematics = Some(LocalResidualAssembly::new(
                KinematicElement {
                    us: model.us,
                    dynamics: d,
                },
                Incidence::new(n_u, 2, krows)?,
                Incidence::new(n_u, 4, kcols.clone())?,
                Incidence::new(n_u, 4, kcols)?,
                Incidence::new(n_u, 1, vec![0; n_u])?,
                state_size,
                design_size,
                options(batch.max(1024)),
            )?);
        }
        let host = BoundaryHost {
            t0,
            ts,
            scale: ks * ts * ls,
            state_size,
            solid_state_size,
            nc,
            fixed_t: fixed_t.clone(),
            times: times.clone(),
            batch_size: batch,
        };
        let weights = |axis: usize, hi: bool| -> (Vec<usize>, Vec<f64>) {
            let counts = triangle_counts(&mesh, grid, axis, hi);
            (
                face_nodes_of(&mesh, grid, axis, hi),
                counts.iter().map(|c| c / 6.0).collect(),
            )
        };
        let boundary = SolidBoundaryHistory::new(host, &p, &tmap, &weights)?;
        Ok(Self {
            p,
            model,
            grid,
            mesh,
            nc,
            nn,
            ne,
            nt,
            times,
            fixed_u,
            fixed_t,
            free_u,
            free_t,
            tmap,
            umap,
            internal_size,
            internal_stop,
            velocity,
            acceleration,
            solid_state_size,
            state_size,
            dynamics,
            compliance_window,
            t_min,
            t_max,
            local,
            inertia,
            kinematics,
            boundary,
            face_counts: Mutex::new(std::collections::BTreeMap::new()),
            last: Mutex::new(None),
        })
    }

    #[must_use]
    pub fn local_size(&self) -> usize {
        self.model.local_size()
    }

    #[must_use]
    pub fn n_t(&self) -> usize {
        self.free_t.len()
    }

    #[must_use]
    pub fn n_u(&self) -> usize {
        self.free_u.len()
    }

    #[must_use]
    pub fn thermal_row_slice(&self) -> std::ops::Range<usize> {
        0..self.n_t()
    }

    #[must_use]
    pub fn displacement_row_slice(&self) -> std::ops::Range<usize> {
        self.n_t()..self.n_t() + self.n_u()
    }

    #[must_use]
    pub fn face_nodes(&self, axis: usize, hi: bool) -> Vec<usize> {
        face_nodes_of(&self.mesh, self.grid, axis, hi)
    }

    #[must_use]
    pub fn face_triangle_counts(&self, axis: usize, hi: bool) -> Vec<f64> {
        let key = (axis, hi);
        if let Ok(cache) = self.face_counts.lock()
            && let Some(v) = cache.get(&key)
        {
            return v.clone();
        }
        let out = triangle_counts(&self.mesh, self.grid, axis, hi);
        if let Ok(mut cache) = self.face_counts.lock() {
            cache.insert(key, out.clone());
        }
        out
    }

    #[must_use]
    pub fn face_weights(&self, axis: usize, hi: bool) -> Vec<f64> {
        self.face_triangle_counts(axis, hi)
            .iter()
            .map(|c| c / 6.0)
            .collect()
    }

    pub fn boundary_weights<S: Scalar>(
        &self,
        axis: usize,
        hi: bool,
        h: &[S; 3],
    ) -> (Vec<usize>, Vec<S>) {
        let other: Vec<usize> = (0..3).filter(|i| *i != axis).collect();
        let face = self.face_nodes(axis, hi);
        let w = self.face_weights(axis, hi);
        (
            face,
            w.iter().map(|x| h[other[0]] * h[other[1]] * *x).collect(),
        )
    }

    fn bc_side(bc: &Value) -> (usize, bool) {
        (usize_of(&bc["axis"]), bc["side"] == json!("hi"))
    }

    pub fn external_force<S: Scalar>(&self, n: usize, h: &[S; 3]) -> Vec<[S; 3]> {
        let mut force = vec![[S::zero(); 3]; self.nn];
        for bc in self.p["tractions"].as_array().into_iter().flatten() {
            let (axis, hi) = Self::bc_side(bc);
            let (face, area) = self.boundary_weights(axis, hi, h);
            for (node, a) in face.iter().zip(&area) {
                for c in 0..3 {
                    force[*node][c] += *a * bc["values"][n][c].as_f64().unwrap_or(f64::NAN);
                }
            }
        }
        if let Some(loads) = self.p.get("nodal_forces_N").filter(|v| !v.is_null()) {
            for (node, row) in force.iter_mut().enumerate() {
                for (c, v) in row.iter_mut().enumerate() {
                    *v += S::from_f64(loads[n][node][c].as_f64().unwrap_or(f64::NAN));
                }
            }
        }
        force
    }

    pub fn boundary_load<S: Scalar>(&self, n: usize, spacing_mm: &[S; 3]) -> Vec<S> {
        let h = spacing_mm.map(|v| v * 1e-3);
        let mut thermal = vec![S::zero(); self.nn];
        let mut force = vec![[S::zero(); 3]; self.nn];
        for bc in self.p["heat_fluxes"].as_array().into_iter().flatten() {
            let (axis, hi) = Self::bc_side(bc);
            let (face, area) = self.boundary_weights(axis, hi, &h);
            let q = bc["values"][n].as_f64().unwrap_or(f64::NAN);
            for (node, a) in face.iter().zip(&area) {
                thermal[*node] -= *a * q;
            }
        }
        for bc in self.p["tractions"].as_array().into_iter().flatten() {
            let (axis, hi) = Self::bc_side(bc);
            let (face, area) = self.boundary_weights(axis, hi, &h);
            for (node, a) in face.iter().zip(&area) {
                for c in 0..3 {
                    force[*node][c] -= *a * bc["values"][n][c].as_f64().unwrap_or(f64::NAN);
                }
            }
        }
        if let Some(loads) = self.p.get("nodal_forces_N").filter(|v| !v.is_null()) {
            for (node, row) in force.iter_mut().enumerate() {
                for (c, v) in row.iter_mut().enumerate() {
                    *v -= S::from_f64(loads[n][node][c].as_f64().unwrap_or(f64::NAN));
                }
            }
        }
        let m = &self.model;
        let mut out = vec![S::zero(); self.state_size];
        for (k, node) in self.free_t.iter().enumerate() {
            out[k] = thermal[*node] / (m.ks * m.ts * m.ls);
        }
        let n_t = self.n_t();
        for (k, dof) in self.free_u.iter().enumerate() {
            out[n_t + k] = force[dof / 3][dof % 3] / (m.ss * m.ls * m.ls);
        }
        out
    }

    #[must_use]
    pub fn local_data(&self, n: usize) -> (Vec<f64>, Vec<f64>) {
        let m = &self.model;
        let width = m.local_width();
        let ls = m.local_size();
        let dt = self.times[n] - self.times[n - 1];
        let bulk = self.p["volumetric_heat_W_m3"][n]
            .as_f64()
            .unwrap_or(f64::NAN);
        let mut current = vec![0.0; self.ne * width];
        let mut previous = vec![0.0; self.ne * width];
        for (e, tet) in self.mesh.tets.iter().enumerate() {
            for (target, index) in [(&mut current, n), (&mut previous, n - 1)] {
                let row = &mut target[e * width..(e + 1) * width];
                for (i, node) in tet.iter().enumerate() {
                    row[i] = (self.fixed_t[index][*node] - m.t0) / m.ts;
                    for c in 0..3 {
                        row[4 + 3 * i + c] = self.fixed_u[index][3 * node + c] / m.us;
                    }
                }
                row[ls] = dt;
                row[ls + 1] = bulk;
                if let Some(mh) = &m.history {
                    row[ls + 2..].copy_from_slice(mh.forcing_row(n, self.mesh.owners[e]));
                }
            }
        }
        (current, previous)
    }

    fn inertia_data(&self, n: usize) -> (Vec<f64>, Vec<f64>) {
        let m = &self.model;
        let mut current = vec![0.0; self.ne * 40];
        let mut previous = vec![0.0; self.ne * 40];
        for (e, tet) in self.mesh.tets.iter().enumerate() {
            for (i, node) in tet.iter().enumerate() {
                current[e * 40 + i] = (self.fixed_t[n][*node] - m.t0) / m.ts;
                previous[e * 40 + i] = (self.fixed_t[n - 1][*node] - m.t0) / m.ts;
            }
        }
        (current, previous)
    }

    fn kinematic_data(&self, n: usize) -> (Vec<f64>, Vec<f64>) {
        let dt = self.times[n] - self.times[n - 1];
        let mut current = vec![0.0; self.n_u() * 4];
        for k in 0..self.n_u() {
            current[4 * k + 3] = dt;
        }
        (current.clone(), current)
    }

    pub fn assembled_residual(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
    ) -> Result<Vec<f64>, CaeError> {
        let (a, b) = self.local_data(n);
        let mut r = self.local.residual(z, old, x, &a, &b)?;
        let spacing = [x[self.nc], x[self.nc + 1], x[self.nc + 2]];
        for (v, l) in r.iter_mut().zip(self.boundary_load(n, &spacing)) {
            *v += l;
        }
        for (v, l) in r.iter_mut().zip(self.boundary.residual(n, z, old, x)?) {
            *v += l;
        }
        if let Some(inertia) = &self.inertia {
            let (a, b) = self.inertia_data(n);
            for (v, l) in r.iter_mut().zip(inertia.residual(z, old, x, &a, &b)?) {
                *v += l;
            }
        }
        if let Some(k) = &self.kinematics {
            let (a, b) = self.kinematic_data(n);
            for (v, l) in r.iter_mut().zip(k.residual(z, old, x, &a, &b)?) {
                *v += l;
            }
        }
        Ok(r)
    }

    pub fn external_nodal_load_residual<S: Scalar>(
        &self,
        force: &[[S; 3]],
    ) -> Result<Vec<S>, CaeError> {
        if force.len() != self.nn {
            return contract("external nodal load shape/type mismatch");
        }
        let m = &self.model;
        let mut out = vec![S::zero(); self.state_size];
        let n_t = self.n_t();
        for (k, dof) in self.free_u.iter().enumerate() {
            out[n_t + k] = -force[dof / 3][dof % 3] / (m.ss * m.ls * m.ls);
        }
        Ok(out)
    }

    pub fn validate_external_nodal_heat(&self, power: &[f64]) -> Result<(), CaeError> {
        if power.len() != self.nn || power.iter().any(|v| !v.is_finite()) {
            return contract("external nodal heat requires finite real [node] incoming power in W");
        }
        Ok(())
    }

    pub fn external_nodal_heat_residual<S: Scalar>(&self, power: &[S]) -> Result<Vec<S>, CaeError> {
        if power.len() != self.nn {
            return contract("external nodal heat shape/type mismatch");
        }
        let m = &self.model;
        let mut out = vec![S::zero(); self.state_size];
        for (k, node) in self.free_t.iter().enumerate() {
            out[k] = -power[*node] / (m.ks * m.ts * m.ls);
        }
        Ok(out)
    }

    pub fn residual_with_external_load(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        force: Option<&[[f64; 3]]>,
        power: Option<&[f64]>,
    ) -> Result<Vec<f64>, CaeError> {
        let mut base = self.assembled_residual(n, z, old, x)?;
        if base.len() != self.state_size {
            return contract("assembled residual size differs from full state layout");
        }
        if let Some(f) = force {
            for (b, v) in base.iter_mut().zip(self.external_nodal_load_residual(f)?) {
                *b += v;
            }
        }
        if let Some(p) = power {
            for (b, v) in base.iter_mut().zip(self.external_nodal_heat_residual(p)?) {
                *b += v;
            }
        }
        Ok(base)
    }

    #[must_use]
    pub fn fields(
        &self,
        n: usize,
        z: &[f64],
        x: &[f64],
    ) -> Vec<crate::solid_elements::ElementFields<f64>> {
        let (data, _) = self.local_data(n.max(1));
        let width = self.model.local_width();
        let mut fixed = data;
        if n == 0 {
            for (e, tet) in self.mesh.tets.iter().enumerate() {
                let row = &mut fixed[e * width..(e + 1) * width];
                for (i, node) in tet.iter().enumerate() {
                    row[i] = (self.fixed_t[0][*node] - self.model.t0) / self.model.ts;
                    for c in 0..3 {
                        row[4 + 3 * i + c] = self.fixed_u[0][3 * node + c] / self.model.us;
                    }
                }
            }
        }
        (0..self.ne)
            .map(|e| {
                let local = self.element_local(n, e, z, &fixed);
                self.model
                    .fields(&self.mesh.gradients[e], &local, &self.element_design(e, x))
            })
            .collect()
    }

    fn sum(parts: Vec<CsrMatrix>) -> Result<CsrMatrix, CaeError> {
        let mut iter = parts.into_iter();
        let Some(mut acc) = iter.next() else {
            return contract("empty Jacobian");
        };
        for m in iter {
            acc = acc
                .add_scaled(1.0, &m, 1.0)
                .map_err(|e| CaeError::contract(e.to_string()))?;
        }
        Ok(implexity_solve::matrix::eliminate_zeros(&acc))
    }

    pub fn jacobian(
        &self,
        kind: Kind,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
    ) -> Result<CsrMatrix, CaeError> {
        let (a, b) = self.local_data(n);
        let mut parts = vec![self.local.jacobian(kind, z, old, x, &a, &b)?];
        if kind == Kind::Design || self.boundary.has_state_terms() {
            parts.extend(self.boundary.jacobians(kind, n, z, old, x)?);
        }
        if let Some(inertia) = &self.inertia {
            let (a, b) = self.inertia_data(n);
            parts.push(inertia.jacobian(kind, z, old, x, &a, &b)?);
        }
        if kind != Kind::Design
            && let Some(k) = &self.kinematics
        {
            let (a, b) = self.kinematic_data(n);
            parts.push(k.jacobian(kind, z, old, x, &a, &b)?);
        }
        if kind == Kind::Design {
            parts.push(self.load_derivative(n, x)?);
        }
        Self::sum(parts)
    }

    fn load_derivative(&self, n: usize, x: &[f64]) -> Result<CsrMatrix, CaeError> {
        let spacing = [x[self.nc], x[self.nc + 1], x[self.nc + 2]];
        let jac = implexity_ad::forward::jacobian::<3, _>(
            |s: &[Dual<3>]| self.boundary_load(n, &[s[0], s[1], s[2]]),
            &spacing,
        )
        .map_err(|e| CaeError::contract(e.to_string()))?;
        let (mut rows, mut cols, mut vals) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..self.state_size {
            for j in 0..3 {
                let v = jac.get(i, j);
                if v != 0.0 {
                    rows.push(i);
                    cols.push(self.nc + j);
                    vals.push(v);
                }
            }
        }
        CsrMatrix::from_triplets(self.state_size, x.len(), &rows, &cols, &vals)
            .map_err(|e| CaeError::contract(e.to_string()))
    }

    pub fn current_action(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        vector: &[f64],
        transpose: bool,
    ) -> Result<Vec<f64>, CaeError> {
        let (a, b) = self.local_data(n);
        let mut r = self
            .local
            .current_action(z, old, x, &a, &b, vector, transpose)?;
        if self.boundary.has_state_terms() {
            for (v, l) in r.iter_mut().zip(
                self.boundary
                    .current_action(n, z, old, x, vector, transpose)?,
            ) {
                *v += l;
            }
        }
        if let Some(inertia) = &self.inertia {
            let (a, b) = self.inertia_data(n);
            for (v, l) in r
                .iter_mut()
                .zip(inertia.current_action(z, old, x, &a, &b, vector, transpose)?)
            {
                *v += l;
            }
        }
        if let Some(k) = &self.kinematics {
            let (a, b) = self.kinematic_data(n);
            for (v, l) in r
                .iter_mut()
                .zip(k.current_action(z, old, x, &a, &b, vector, transpose)?)
            {
                *v += l;
            }
        }
        Ok(r)
    }

    #[must_use]
    pub fn nodal_temperature(&self, n: usize, z: &[f64]) -> Vec<f64> {
        let mut t = self.fixed_t[n].clone();
        for (k, node) in self.free_t.iter().enumerate() {
            t[*node] = self.model.t0 + self.model.ts * z[k];
        }
        t
    }

    #[must_use]
    pub fn nodal_displacement(&self, n: usize, z: &[f64]) -> Vec<[f64; 3]> {
        let mut u = self.fixed_u[n].clone();
        let n_t = self.n_t();
        for (k, dof) in self.free_u.iter().enumerate() {
            u[*dof] = self.model.us * z[n_t + k];
        }
        u.chunks(3).map(|c| [c[0], c[1], c[2]]).collect()
    }

    #[must_use]
    pub fn kinematic_nodes(&self, z: &[f64]) -> (Vec<[f64; 3]>, Vec<[f64; 3]>) {
        let mut v = vec![0.0; self.nn * 3];
        let mut a = vec![0.0; self.nn * 3];
        if let Some(d) = &self.dynamics {
            for (k, dof) in self.free_u.iter().enumerate() {
                v[*dof] = d.vs * z[self.velocity.start + k];
                a[*dof] = d.acs * z[self.acceleration.start + k];
            }
        }
        let triples = |x: Vec<f64>| x.chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
        (triples(v), triples(a))
    }

    #[must_use]
    pub fn element_local(&self, n: usize, e: usize, z: &[f64], data: &[f64]) -> Vec<f64> {
        let width = self.model.local_width();
        let mut row = data[e * width..(e + 1) * width].to_vec();
        let tet = &self.mesh.tets[e];
        for (i, node) in tet.iter().enumerate() {
            if let Ok(k) = usize::try_from(self.tmap[*node]) {
                row[i] = z[k];
            }
            for c in 0..3 {
                if let Ok(k) = usize::try_from(self.umap[3 * node + c]) {
                    row[4 + 3 * i + c] = z[k];
                }
            }
        }
        let start = self.n_t() + self.n_u() + e * self.internal_size;
        row[16..16 + self.internal_size].copy_from_slice(&z[start..start + self.internal_size]);
        let _ = n;
        row
    }

    #[must_use]
    pub fn element_design(&self, e: usize, x: &[f64]) -> [f64; 5] {
        let o = self.mesh.owners[e];
        [
            x[o],
            x[self.nc],
            x[self.nc + 1],
            x[self.nc + 2],
            x[self.nc + 3 + o],
        ]
    }

    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn material_validity(&self, n: usize, z: &[f64], x: &[f64]) -> Value {
        let t = self.nodal_temperature(n, z);
        let finite = t.iter().all(Scalar::is_finite);
        let rho = &x[..self.nc];
        let c = &x[self.nc + 3..];
        let cell_a: Vec<f64> = rho.iter().zip(c).map(|(r, c)| r * (1.0 - c)).collect();
        let cell_b: Vec<f64> = rho.iter().zip(c).map(|(r, c)| r * c).collect();
        let [ma, mb] = &self.model.materials;
        let mut stats = ValidityStats::default();
        for (e, tet) in self.mesh.tets.iter().enumerate() {
            let owner = self.mesh.owners[e];
            let active_a = cell_a[owner] > 0.0;
            let active_b = cell_b[owner] > 0.0;
            for node in tet {
                let te = t[*node];
                let outside_a = te < ma.t_min || te > ma.t_max || !te.is_finite();
                let outside_b = te < mb.t_min || te > mb.t_max || !te.is_finite();
                stats.add(active_a, active_b, outside_a, outside_b, te);
            }
        }
        let phase_aware = self.model.numerical;
        let legacy_violation = t
            .iter()
            .any(|v| !v.is_finite() || *v < self.t_min || *v > self.t_max);
        let passed = if phase_aware {
            finite && stats.violations[0] == 0 && stats.violations[1] == 0
        } else {
            !legacy_violation
        };
        let fixed_values: Vec<f64> = self.p["temperature_bcs"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|bc| bc["values"][n].as_f64().unwrap_or(f64::NAN))
            .collect();
        let fixed_common = fixed_values.is_empty()
            || (fixed_values.iter().all(Scalar::is_finite)
                && fixed_values.iter().copied().fold(f64::INFINITY, f64::min) >= self.t_min
                && fixed_values
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max)
                    <= self.t_max);
        let policy = self
            .p
            .get("inactive_phase_numerical_material")
            .filter(|v| !v.is_null());
        let pv = |k: &str| policy.map_or(Value::Null, |p| p[k].clone());
        let opt = |v: f64| if v.is_finite() { json!(v) } else { Value::Null };
        let nodes = 4 * self.ne;
        json!({
            "solid_temperature_material_interval_screen_passed": passed,
            "solid_temperature_material_interval_screen_semantics": if phase_aware { "positive_physical_solid_endmember_support_at_every_element_node" } else { "all_nodes_common_material_interval_legacy" },
            "all_nodal_temperature_finite_screen_passed": finite,
            "all_nodal_temperature_min_K": if finite { json!(t.iter().copied().fold(f64::INFINITY, f64::min)) } else { Value::Null },
            "all_nodal_temperature_max_K": if finite { json!(t.iter().copied().fold(f64::NEG_INFINITY, f64::max)) } else { Value::Null },
            "authored_temperature_bc_common_interval_diagnostic_passed": fixed_common,
            "authored_temperature_bc_separate_common_interval_gate_applied": !phase_aware,
            "positive_physical_solid_cell_count": rho.iter().filter(|r| **r > 0.0).count(),
            "exact_zero_physical_solid_ghost_cell_count": rho.iter().filter(|r| **r == 0.0).count(),
            "positive_endmember_0_support_cell_count": cell_a.iter().filter(|r| **r > 0.0).count(),
            "positive_endmember_1_support_cell_count": cell_b.iter().filter(|r| **r > 0.0).count(),
            "positive_endmember_0_support_element_node_count": stats.active_nodes[0],
            "positive_endmember_1_support_element_node_count": stats.active_nodes[1],
            "exact_zero_endmember_0_support_element_node_count": nodes - stats.active_nodes[0],
            "exact_zero_endmember_1_support_element_node_count": nodes - stats.active_nodes[1],
            "exact_zero_physical_solid_ghost_element_node_count": 4 * 6 * rho.iter().filter(|r| **r == 0.0).count(),
            "active_endmember_0_temperature_min_K": stats.extreme(0, true).map_or(Value::Null, opt),
            "active_endmember_0_temperature_max_K": stats.extreme(0, false).map_or(Value::Null, opt),
            "active_endmember_1_temperature_min_K": stats.extreme(1, true).map_or(Value::Null, opt),
            "active_endmember_1_temperature_max_K": stats.extreme(1, false).map_or(Value::Null, opt),
            "active_endmember_0_material_interval_violation_element_node_count": stats.violations[0],
            "active_endmember_1_material_interval_violation_element_node_count": stats.violations[1],
            "inactive_endmember_0_outside_interval_ghost_element_node_count": stats.ghosts[0],
            "inactive_endmember_1_outside_interval_ghost_element_node_count": stats.ghosts[1],
            "inactive_phase_numerical_material_enabled": phase_aware,
            "inactive_phase_numerical_material_schema": pv("schema"),
            "inactive_phase_numerical_material_method": pv("method"),
            "inactive_phase_numerical_material_scope": pv("scope"),
            "inactive_phase_numerical_material_provenance": pv("provenance"),
            "physical_material_extrapolation_authorized": self.p["applicability_policy"].as_str() == Some("report_only"),
            "numerical_continuation_scope": if self.p["applicability_policy"].as_str() == Some("report_only") { "all_positive_temperature_states_for_optimization_exploration" } else { "inactive_phase_only" },
            "applicability_policy": self.p.get("applicability_policy").cloned().unwrap_or(json!("enforce")),
            "physical_qualification": false,
        })
    }

    pub fn check(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> Result<(), CaeError> {
        if z.iter().chain(old).any(|v| !v.is_finite()) {
            return convergence("non-finite solid history state");
        }
        let validity = self.material_validity(n, z, x);
        let report_only = self.p["applicability_policy"].as_str() == Some("report_only");
        if report_only
            && self
                .nodal_temperature(n, z)
                .iter()
                .any(|t| !t.is_finite() || *t <= 0.0)
        {
            return convergence("solid temperature must be finite and positive");
        }
        if !report_only
            && validity["solid_temperature_material_interval_screen_passed"] != json!(true)
        {
            let t = self.nodal_temperature(n, z);
            let argmin = t
                .iter()
                .enumerate()
                .fold(0, |b, (i, v)| if *v < t[b] { i } else { b });
            let argmax = t
                .iter()
                .enumerate()
                .fold(0, |b, (i, v)| if *v > t[b] { i } else { b });
            let show = |v: &Value| implexity_core::pyobj::py_str(v);
            return convergence(format!(
                "temperature leaves authored material validity interval; history_step={n}; nodal_temperature_min_K={}; nodal_temperature_min_index={argmin}; nodal_temperature_max_K={}; nodal_temperature_max_index={argmax}; active_endmember_0_violation_element_nodes={}; active_endmember_1_violation_element_nodes={}; authored_temperature_bc_common_interval_diagnostic_passed={}",
                show_float(&validity["all_nodal_temperature_min_K"]),
                show_float(&validity["all_nodal_temperature_max_K"]),
                show(
                    &validity["active_endmember_0_material_interval_violation_element_node_count"]
                ),
                show(
                    &validity["active_endmember_1_material_interval_violation_element_node_count"]
                ),
                if validity["authored_temperature_bc_common_interval_diagnostic_passed"]
                    == json!(true)
                {
                    "True"
                } else {
                    "False"
                },
            ));
        }
        self.boundary.check(n, z)
    }

    #[must_use]
    pub fn initial_material_state(&self) -> Vec<f64> {
        let mut z = vec![0.0; self.state_size];
        let m = &self.model;
        let base = self.n_t() + self.n_u();
        for e in 0..self.ne {
            let row = &mut z[base + e * self.internal_size..base + (e + 1) * self.internal_size];
            if let Some(v) = &m.viscoelastic {
                let range = m.layout.viscoelastic();
                for ((slot, init), scale) in row[range].iter_mut().zip(v.initial()).zip(v.scales())
                {
                    *slot = init / scale;
                }
            }
            if let Some(h) = &m.history {
                let start = m.layout.material_start();
                for (k, (init, scale)) in h.initial.iter().zip(&h.scales).enumerate() {
                    row[start + k] = init / scale;
                }
            }
        }
        z
    }

    #[must_use]
    pub fn initial_state(&self) -> Vec<f64> {
        self.boundary.initial_state(self.initial_material_state())
    }

    pub fn solve(self: &Arc<Self>, x: &[f64]) -> Result<Arc<HistorySolution>, CaeError> {
        let ident: Vec<u64> = x.iter().map(|v| v.to_bits()).collect();
        if let Ok(last) = self.last.lock()
            && let Some((key, sol)) = last.as_ref()
            && *key == ident
        {
            return Ok(Arc::clone(sol));
        }
        let numerics = &self.p["numerics"];
        let options = HistoryOptions {
            local_elimination_partition: self.local_elimination_partition(self.state_size)?,
            tolerance: f(numerics, "tolerance"),
            max_iterations: crate::util::count(&numerics["max_iterations"]),
            ..HistoryOptions::default()
        };
        let system = NativeHistorySystem::new(Arc::new(KernelProblem(Arc::clone(self))), options)?;
        let sol = system.solve(
            x,
            &self.initial_state(),
            self.nt - 1,
            &HistorySolveOptions::default(),
        )?;
        self.validate_history(&sol, x)?;
        let sol = Arc::new(sol);
        if let Ok(mut last) = self.last.lock() {
            *last = Some((ident, Arc::clone(&sol)));
        }
        Ok(sol)
    }

    pub fn system(self: &Arc<Self>) -> Result<NativeHistorySystem, CaeError> {
        let numerics = &self.p["numerics"];
        let options = HistoryOptions {
            local_elimination_partition: self.local_elimination_partition(self.state_size)?,
            tolerance: f(numerics, "tolerance"),
            max_iterations: crate::util::count(&numerics["max_iterations"]),
            ..HistoryOptions::default()
        };
        NativeHistorySystem::new(Arc::new(KernelProblem(Arc::clone(self))), options)
    }

    pub fn validate_history(&self, sol: &HistorySolution, x: &[f64]) -> Result<(), CaeError> {
        let m = &self.model;
        let base = self.n_t() + self.n_u();
        let mut minimum_dissipation = f64::INFINITY;
        let mut maxstrain = 0.0_f64;
        if let Some(h) = &m.history {
            for state in &sol.states {
                let mut aux = Vec::with_capacity(self.ne * h.size);
                for e in 0..self.ne {
                    let start = base + e * self.internal_size + m.layout.material_start();
                    aux.extend(
                        state[start..start + h.size]
                            .iter()
                            .zip(&h.scales)
                            .map(|(v, s)| v * s),
                    );
                }
                h.check_state(&aux)?;
            }
        }
        for n in 1..self.nt {
            self.check(n, &sol.states[n], &sol.states[n - 1], x)?;
            let d = self.observe(n, &sol.states[n], &sol.states[n - 1], x);
            if let Some(v) = &m.viscoelastic {
                let range = m.layout.viscoelastic();
                let mut q = Vec::new();
                for e in 0..self.ne {
                    let start = base + e * self.internal_size;
                    let row = &sol.states[n][start..start + self.internal_size];
                    q.extend(
                        row[range.clone()]
                            .iter()
                            .zip(&m.scales[range.clone()])
                            .map(|(a, s)| a * s),
                    );
                }
                v.check_state(&q, &d.material_temperature)?;
                if d.polymer.iter().flatten().any(|v| !v.is_finite()) {
                    return convergence("non-finite viscoelastic diagnostic");
                }
                minimum_dissipation = minimum_dissipation.min(
                    d.polymer_column("dissipation_increment_J_m3")
                        .into_iter()
                        .fold(f64::INFINITY, f64::min),
                );
                if d.polymer_column("strain_margin")
                    .into_iter()
                    .fold(f64::INFINITY, f64::min)
                    < 0.0
                {
                    return convergence("viscoelastic strain exceeds authored applicability limit");
                }
            }
            let checked = d
                .strain
                .iter()
                .flatten()
                .chain(&d.plastic_dissipation)
                .chain(&d.creep_dissipation)
                .chain(&d.equivalent_plastic)
                .chain(&d.equivalent_creep);
            if checked.clone().any(|v| !v.is_finite()) {
                return convergence("non-finite solid constitutive validity diagnostic");
            }
            maxstrain = maxstrain.max(d.strain.iter().flatten().fold(0.0, |a, v| a.max(v.abs())));
            minimum_dissipation = minimum_dissipation
                .min(
                    d.plastic_dissipation
                        .iter()
                        .copied()
                        .fold(f64::INFINITY, f64::min),
                )
                .min(
                    d.creep_dissipation
                        .iter()
                        .copied()
                        .fold(f64::INFINITY, f64::min),
                );
            let min_p = d
                .equivalent_plastic
                .iter()
                .copied()
                .fold(f64::INFINITY, f64::min);
            let min_c = d
                .equivalent_creep
                .iter()
                .copied()
                .fold(f64::INFINITY, f64::min);
            if min_p < -1e-10 || min_c < -1e-10 {
                return convergence("negative accumulated inelastic strain");
            }
        }
        if self.p["applicability_policy"].as_str() != Some("report_only")
            && maxstrain > f(&self.p["numerics"], "max_small_strain")
        {
            return convergence("small-strain kinematic validity limit exceeded");
        }
        if minimum_dissipation < -1e-3 {
            return convergence("constitutive integration produced negative dissipation");
        }
        Ok(())
    }
}

fn show_float(v: &Value) -> String {
    match v.as_f64() {
        Some(x) => implexity_core::py_repr::repr_float(x),
        None => "None".into(),
    }
}

#[derive(Default)]
struct ValidityStats {
    active_nodes: [usize; 2],
    violations: [usize; 2],
    ghosts: [usize; 2],
    min: [f64; 2],
    max: [f64; 2],
    seen: [bool; 2],
}

impl ValidityStats {
    #[allow(clippy::fn_params_excessive_bools)]
    fn add(&mut self, active_a: bool, active_b: bool, outside_a: bool, outside_b: bool, t: f64) {
        for (k, (active, outside)) in [(active_a, outside_a), (active_b, outside_b)]
            .into_iter()
            .enumerate()
        {
            if active {
                self.active_nodes[k] += 1;
                if outside {
                    self.violations[k] += 1;
                }
                if self.seen[k] {
                    self.min[k] = if t < self.min[k] || t.is_nan() {
                        t
                    } else {
                        self.min[k]
                    };
                    self.max[k] = if t > self.max[k] || t.is_nan() {
                        t
                    } else {
                        self.max[k]
                    };
                } else {
                    self.min[k] = t;
                    self.max[k] = t;
                    self.seen[k] = true;
                }
            } else if outside {
                self.ghosts[k] += 1;
            }
        }
    }

    fn extreme(&self, k: usize, minimum: bool) -> Option<f64> {
        self.seen[k].then(|| if minimum { self.min[k] } else { self.max[k] })
    }
}

pub struct KernelProblem(pub Arc<SolidKernel>);

impl HistoryProblem for KernelProblem {
    fn residual(&self, n: usize, z: &[f64], prev: &[f64], x: &[f64]) -> Result<Vec<f64>, CaeError> {
        self.0.check(n, z, prev, x)?;
        self.0.assembled_residual(n, z, prev, x)
    }
    fn state_jacobian(
        &self,
        n: usize,
        z: &[f64],
        prev: &[f64],
        x: &[f64],
    ) -> Result<Jacobian, CaeError> {
        self.0
            .jacobian(Kind::Current, n, z, prev, x)
            .map(Jacobian::Csr)
    }
    fn previous_jacobian(
        &self,
        n: usize,
        z: &[f64],
        prev: &[f64],
        x: &[f64],
    ) -> Result<Jacobian, CaeError> {
        self.0
            .jacobian(Kind::Previous, n, z, prev, x)
            .map(Jacobian::Csr)
    }
    fn design_jacobian(
        &self,
        n: usize,
        z: &[f64],
        prev: &[f64],
        x: &[f64],
    ) -> Result<Jacobian, CaeError> {
        self.0
            .jacobian(Kind::Design, n, z, prev, x)
            .map(Jacobian::Csr)
    }
}
