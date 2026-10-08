// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use implexity_ad::Scalar;
use implexity_core::CaeError;
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::local_assembly::{
    AssemblyOptions, Incidence, Kind, LocalResidual, LocalResidualAssembly,
};
use serde_json::{Value, json};

use crate::solid_face_trace::{Side, SolidFaceTrace};
use crate::solid_history::SolidKernel;

pub const COMPONENT_KIND: &str = "shared_phase_stress_load";
pub const COMPONENT_ID: &str = "density_jump_total_stress";

#[must_use]
pub fn runtime_support() -> Value {
    json!({"status": "field_component", "history": true, "data": "user_required",
        "limitations": LIMITATIONS})
}

pub const LIMITATIONS: [&str; 3] = [
    "Density-jump internal-face loads from reconstructed absolute pressure and total viscous stress.",
    "Not moving-mesh FSI; diffuse/load/Brinkman regularisation must be refined together.",
    "External solid-face loads remain explicitly authored; no assumed external pressure.",
];

pub trait MacFluid: Send + Sync + 'static {
    fn grid(&self) -> [usize; 3];
    fn nc(&self) -> usize;
    fn nv(&self) -> usize;
    fn cell(&self, cell: [usize; 3]) -> usize;
    fn face(&self, axis: usize, face: [usize; 3]) -> i64;
    fn us(&self) -> f64;
    fn t0(&self) -> f64;
    fn ts(&self) -> f64;
    fn pref(&self) -> f64;
    fn ps(&self) -> f64;
    fn viscosity<S: Scalar>(&self, temperature: S) -> S;
}


#[must_use]
pub fn velocity_gradient_map<F: MacFluid + ?Sized>(fluid: &F) -> Vec<Vec<(usize, f64)>> {
    let grid = fluid.grid();
    let mut rows: Vec<BTreeMap<usize, f64>> = vec![BTreeMap::new(); 9 * fluid.nc()];
    let mut add = |row: usize, idx: i64, w: f64| {
        if let Ok(idx) = usize::try_from(idx)
            && w != 0.0
        {
            *rows[row].entry(idx).or_insert(0.0) += w;
        }
    };
    for c0 in 0..grid[0] {
        for c1 in 0..grid[1] {
            for c2 in 0..grid[2] {
                let cell = [c0, c1, c2];
                let i = fluid.cell(cell);
                for a in 0..3 {
                    for b in 0..3 {
                        let row = 9 * i + 3 * a + b;
                        if a == b {
                            let mut hi = cell;
                            hi[a] += 1;
                            add(row, fluid.face(a, cell), -1.0);
                            add(row, fluid.face(a, hi), 1.0);
                        } else if grid[b] > 1 {
                            let (mut lo, mut hi) = (cell, cell);
                            lo[b] = cell[b].saturating_sub(1);
                            hi[b] = (cell[b] + 1).min(grid[b] - 1);
                            let distance = (hi[b] - lo[b]) as f64;
                            for (ix, sgn) in [(lo, -1.0), (hi, 1.0)] {
                                let mut jj = ix;
                                jj[a] += 1;
                                add(row, fluid.face(a, ix), sgn * 0.5 / distance);
                                add(row, fluid.face(a, jj), sgn * 0.5 / distance);
                            }
                        }
                    }
                }
            }
        }
    }
    rows.into_iter().map(|r| r.into_iter().collect()).collect()
}

#[derive(Debug, Clone)]
pub struct FaceRecord {
    pub axis: usize,
    pub cells: [usize; 2],
    pub nodes: [usize; 4],
    pub weights: [f64; 4],
}

struct FaceData {
    gradient: Vec<f64>,
    axis: usize,
    weights: [f64; 4],
}

struct TractionKernel<F: MacFluid> {
    fluid: Arc<F>,
    faces: Vec<FaceData>,
    maxw: usize,
    ss: f64,
    ls: f64,
}

impl<F: MacFluid> TractionKernel<F> {
    fn traction<S: Scalar>(&self, item: usize, z: &[S], x: &[S]) -> [S; 3] {
        let d = &self.faces[item];
        let f = &self.fluid;
        let h = [x[2] * 1e-3, x[3] * 1e-3, x[4] * 1e-3];
        let w = self.maxw;
        let mut out = [S::zero(); 3];
        for c in 0..2 {
            let mut grad = [[S::zero(); 3]; 3];
            for (a, row) in grad.iter_mut().enumerate() {
                for (b, g) in row.iter_mut().enumerate() {
                    let base = ((c * 3 + a) * 3 + b) * w;
                    let mut acc = S::zero();
                    for v in 0..w {
                        let coefficient = d.gradient[base + v];
                        if coefficient != 0.0 {
                            acc += z[v] * coefficient;
                        }
                    }
                    *g = acc * f.us() / h[b];
                }
            }
            let temperature = z[w + 2 + c] * f.ts() + f.t0();
            let mu = f.viscosity(temperature);
            let pressure = z[w + c] * f.ps() + f.pref();
            for (a, o) in out.iter_mut().enumerate() {
                let b = d.axis;
                let mut s = mu * (grad[a][b] + grad[b][a]);
                if a == b {
                    s -= pressure;
                }
                *o += s * 0.5;
            }
        }
        out
    }
}

impl<F: MacFluid> LocalResidual for TractionKernel<F> {
    fn residual<S: Scalar>(&self, item: usize, current: &[S], _previous: &[S], design: &[S], out: &mut [S]) {
        let d = &self.faces[item];
        let h = [design[2] * 1e-3, design[3] * 1e-3, design[4] * 1e-3];
        let area = h[0] * h[1] * h[2] / h[d.axis];
        let traction = self.traction(item, current, design);
        let scale = self.ss * self.ls * self.ls;
        for (n, weight) in d.weights.iter().enumerate() {
            for a in 0..3 {
                out[3 * n + a] = -(traction[a] * (design[0] - design[1]) * area) * *weight / scale;
            }
        }
    }
}

pub struct DensityJumpStressTransfer<F: MacFluid> {
    kernel: Arc<TractionKernel<F>>,
    pub records: Vec<FaceRecord>,
    pub local: LocalResidualAssembly<ArcKernel<F>>,
    inc: Vec<i64>,
    design: Vec<usize>,
    width: usize,
    nc: usize,
    nn: usize,
}

pub struct ArcKernel<F: MacFluid>(Arc<TractionKernel<F>>);

impl<F: MacFluid> LocalResidual for ArcKernel<F> {
    fn residual<S: Scalar>(&self, item: usize, current: &[S], previous: &[S], design: &[S], out: &mut [S]) {
        self.0.residual(item, current, previous, design, out);
    }
}

#[derive(Debug, Clone)]
pub struct TransferObservables {
    pub traction_pa: Vec<[f64; 3]>,
    pub signed_density_jump: Vec<f64>,
    pub face_area_m2: Vec<f64>,
    pub interface_force_n: Vec<[f64; 3]>,
    pub solid_nodal_force_n: Vec<[f64; 3]>,
    pub nodal_minus_face_resultant_n: [f64; 3],
}

impl<F: MacFluid> DensityJumpStressTransfer<F> {

    #[allow(clippy::too_many_lines)]
    pub fn new(
        solid: &SolidKernel,
        fluid: Arc<F>,
        solid_slice: Range<usize>,
        fluid_slice: Range<usize>,
        state_size: usize,
        design_size: usize,
    ) -> Result<Self, CaeError> {
        let s = solid;
        if s.grid != fluid.grid() {
            return Err(CaeError::contract("density-jump transfer requires a common Cartesian grid"));
        }
        if solid_slice.len() != s.state_size {
            return Err(CaeError::contract(
                "solid residual slice disagrees with the solid kernel state size",
            ));
        }
        let drs = s.displacement_row_slice();
        let nu = s.n_u();
        let mut umap = vec![-1i64; s.nn * 3];
        for (k, dof) in s.free_u.iter().enumerate() {
            umap[*dof] = i64::try_from(solid_slice.start + drs.start + k).unwrap_or(-1);
        }
        debug_assert_eq!(drs.len(), nu);
        let gmap = velocity_gradient_map(fluid.as_ref());
        let trace = SolidFaceTrace::new(s.grid, &s.mesh.ijk, &s.mesh.tets, &s.mesh.owners)?;
        let shape = [s.grid[0] + 1, s.grid[1] + 1, s.grid[2] + 1];
        let node_id = |q: [usize; 3]| (q[0] * shape[1] + q[1]) * shape[2] + q[2];
        let (nc, nv) = (fluid.nc(), fluid.nv());
        let offset = |v: usize| i64::try_from(fluid_slice.start + v).unwrap_or(-1);
        struct Raw {
            record: FaceRecord,
            vids: Vec<usize>,
            b: Vec<Vec<f64>>,
        }
        let mut raw: Vec<Raw> = Vec::new();
        let mut maxw = 0;
        for axis in 0..3 {
            let tang: Vec<usize> = (0..3).filter(|b| *b != axis).collect();
            for c0 in 0..s.grid[0] {
                for c1 in 0..s.grid[1] {
                    for c2 in 0..s.grid[2] {
                        let cell = [c0, c1, c2];
                        if cell[axis] + 1 >= s.grid[axis] {
                            continue;
                        }
                        let mut other = cell;
                        other[axis] += 1;
                        let pair = [fluid.cell(cell), fluid.cell(other)];
                        let grows: Vec<&Vec<(usize, f64)>> = pair
                            .iter()
                            .flat_map(|p| (0..9).map(move |k| 9 * p + k))
                            .map(|r| &gmap[r])
                            .collect();
                        let vids: Vec<usize> = grows
                            .iter()
                            .flat_map(|r| r.iter().map(|(c, _)| *c))
                            .collect::<std::collections::BTreeSet<_>>()
                            .into_iter()
                            .collect();
                        let b: Vec<Vec<f64>> = grows
                            .iter()
                            .map(|r| {
                                vids.iter()
                                    .map(|v| r.iter().find(|(c, _)| c == v).map_or(0.0, |(_, w)| *w))
                                    .collect()
                            })
                            .collect();
                        let mut nodes = [0usize; 4];
                        for (slot, (j, k)) in [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().enumerate() {
                            let mut q = cell;
                            q[axis] += 1;
                            q[tang[0]] += j;
                            q[tang[1]] += k;
                            nodes[slot] = node_id(q);
                        }
                        let weights = trace.nodal_weights(cell, axis, Side::Hi, nodes)?;
                        maxw = maxw.max(vids.len());
                        raw.push(Raw { record: FaceRecord { axis, cells: pair, nodes, weights }, vids, b });
                    }
                }
            }
        }
        if raw.is_empty() {
            return Err(CaeError::contract(
                "shared-domain stress reconstruction requires at least two cells",
            ));
        }
        let width = maxw + 4;
        let (mut rows, mut inc, mut design, mut faces, mut records) =
            (vec![], vec![], vec![], vec![], vec![]);
        for r in raw {
            let rec = r.record;
            for node in rec.nodes {
                for c in 0..3 {
                    rows.push(umap[3 * node + c]);
                }
            }
            inc.extend(r.vids.iter().map(|v| offset(*v)));
            inc.extend(std::iter::repeat_n(-1, maxw - r.vids.len()));
            for p in rec.cells {
                inc.push(offset(nv + p));
            }
            for p in rec.cells {
                inc.push(offset(nv + nc + p));
            }
            design.extend([rec.cells[0], rec.cells[1], s.nc, s.nc + 1, s.nc + 2]);
            let mut gradient = vec![0.0; 18 * maxw];
            for (row, values) in r.b.iter().enumerate() {
                gradient[row * maxw..row * maxw + values.len()].copy_from_slice(values);
            }
            faces.push(FaceData { gradient, axis: rec.axis, weights: rec.weights });
            records.push(rec);
        }
        let count = records.len();
        let kernel = Arc::new(TractionKernel { fluid, faces, maxw, ss: s.model.ss, ls: s.model.ls });
        let to_i64 = |v: &[usize]| v.iter().map(|x| i64::try_from(*x).unwrap_or(-1)).collect::<Vec<_>>();
        let batch = s.p["assembly"]["batch_size"].as_u64().and_then(|b| usize::try_from(b).ok()).unwrap_or(1);
        let local = LocalResidualAssembly::new(
            ArcKernel(Arc::clone(&kernel)),
            Incidence::new(count, 12, rows)?,
            Incidence::new(count, width, inc.clone())?,
            Incidence::new(count, width, inc.clone())?,
            Incidence::new(count, 5, to_i64(&design))?,
            state_size,
            design_size,
            AssemblyOptions { batch_size: batch.max(1), ..AssemblyOptions::default() },
        )?;
        Ok(Self { kernel, records, local, inc, design, width, nc: s.nc, nn: s.nn })
    }

    fn zeros(&self) -> Vec<f64> {
        vec![0.0; self.inc.len()]
    }


    pub fn residual(&self, _n: usize, z: &[f64], old: &[f64], x: &[f64]) -> Result<Vec<f64>, CaeError> {
        let zero = self.zeros();
        self.local.residual(z, old, x, &zero, &zero)
    }


    pub fn jacobian(
        &self,
        kind: Kind,
        _n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
    ) -> Result<CsrMatrix, CaeError> {
        let zero = self.zeros();
        self.local.jacobian(kind, z, old, x, &zero, &zero)
    }


    pub fn current_action(
        &self,
        _n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        vector: &[f64],
        transpose: bool,
    ) -> Result<Vec<f64>, CaeError> {
        let zero = self.zeros();
        self.local.current_action(z, old, x, &zero, &zero, vector, transpose)
    }

    #[must_use]
    pub fn observables(&self, z: &[f64], x: &[f64]) -> TransferObservables {
        let count = self.records.len();
        let mut out = TransferObservables {
            traction_pa: Vec::with_capacity(count),
            signed_density_jump: Vec::with_capacity(count),
            face_area_m2: Vec::with_capacity(count),
            interface_force_n: Vec::with_capacity(count),
            solid_nodal_force_n: vec![[0.0; 3]; self.nn],
            nodal_minus_face_resultant_n: [0.0; 3],
        };
        let h = [x[self.nc] * 1e-3, x[self.nc + 1] * 1e-3, x[self.nc + 2] * 1e-3];
        for (e, rec) in self.records.iter().enumerate() {
            let state: Vec<f64> = self.inc[e * self.width..(e + 1) * self.width]
                .iter()
                .map(|i| usize::try_from(*i).map_or(0.0, |i| z[i]))
                .collect();
            let d: Vec<f64> = self.design[e * 5..(e + 1) * 5].iter().map(|i| x[*i]).collect();
            let traction = self.kernel.traction(e, &state, &d);
            let area = h[0] * h[1] * h[2] / h[rec.axis];
            let jump = d[0] - d[1];
            let force = traction.map(|t| t * jump * area);
            for (node, w) in rec.nodes.iter().zip(rec.weights) {
                for a in 0..3 {
                    out.solid_nodal_force_n[*node][a] += w * force[a];
                }
            }
            out.traction_pa.push(traction);
            out.signed_density_jump.push(jump);
            out.face_area_m2.push(area);
            out.interface_force_n.push(force);
        }
        for a in 0..3 {
            let nodal: f64 = out.solid_nodal_force_n.iter().map(|f| f[a]).sum();
            let face: f64 = out.interface_force_n.iter().map(|f| f[a]).sum();
            out.nodal_minus_face_resultant_n[a] = nodal - face;
        }
        out
    }
}
