// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

use super::caloric::SharedFluidCaloric;
use super::lattice::{Collision, Grid, Q, collision, physical_scales};
use super::sp::{self, Sp, SpMap};


pub fn cell_center_map(grid: Grid, ijk: &[[usize; 3]]) -> CaeResult<Sp> {
    let ns = grid.n.map(|v| v + 1);
    let nn = ns[0] * ns[1] * ns[2];
    let expected =
        (0..nn).all(|i| ijk.get(i) == Some(&[i / (ns[1] * ns[2]), (i / ns[2]) % ns[1], i % ns[2]]));
    if ijk.len() != nn || !expected {
        return Err(CaeError::contract(
            "temperature restriction requires the declared C-order Cartesian node coordinates",
        ));
    }
    let (mut rows, mut cols) = (Vec::new(), Vec::new());
    for c in 0..grid.cells() {
        let [i, j, k] = grid.ijk(c);
        for corner in 0..8 {
            rows.push(c);
            cols.push(((i + (corner >> 2 & 1)) * ns[1] + j + (corner >> 1 & 1)) * ns[2] + k + (corner & 1));
        }
    }
    sp::triplets(grid.cells(), nn, &rows, &cols, &vec![0.125; rows.len()])
}

#[derive(Clone, Debug)]
pub struct SharedDragAdapter {
    pub q: Sp,
    qmap: SpMap,
}

#[derive(Clone, Debug)]
pub struct Drag<S> {
    pub cells: Vec<Collision<S>>,
    pub post: Vec<S>,
    pub fluid_drag_force_n: Vec<[S; 3]>,
    pub solid_reaction_force_n: Vec<[S; 3]>,
    pub drag_dissipation_w: Vec<S>,
    pub fluid_drag_work_w: Vec<S>,
    pub solid_drag_work_w: Vec<S>,
    pub solid_nodal_force_n: Vec<[S; 3]>,
    pub shared_nodal_heat_w: Vec<S>,
}

impl SharedDragAdapter {

    pub fn new(grid: Grid, ijk: &[[usize; 3]]) -> CaeResult<Self> {
        let q = cell_center_map(grid, ijk)?;
        #[allow(clippy::cast_precision_loss)]
        for a in 0..3 {
            let coord: Vec<f64> = ijk.iter().map(|p| p[a] as f64).collect();
            let centre = sp::mv(&q, &coord)?;
            if centre.iter().enumerate().any(|(c, v)| (v - (grid.ijk(c)[a] as f64 + 0.5)).abs() > 1e-14) {
                return Err(CaeError::contract("mechanical transfer moment mismatch"));
            }
        }
        Ok(Self { qmap: SpMap::new(&q), q })
    }

    #[must_use]
    pub fn qmap(&self) -> &SpMap {
        &self.qmap
    }

    #[must_use]
    pub fn solid_cell_velocity<S: Scalar>(&self, nodal: &[[S; 3]]) -> Vec<[S; 3]> {
        self.qmap.apply_vec(nodal)
    }

    #[must_use]
    pub fn coupled<S: Scalar>(
        &self,
        caloric: &SharedFluidCaloric,
        f: &[S],
        q: &[S],
        g: &[[S; 3]],
        tau: &[S],
        nodal_velocity: &[[S; 3]],
        beta: &[S],
        rho: f64,
        h: f64,
        dt: f64,
    ) -> Drag<S> {
        let us: Vec<[S; 3]> =
            self.solid_cell_velocity(nodal_velocity).iter().map(|v| v.map(|x| x * dt / h)).collect();
        let nc = q.len();
        let cells: Vec<Collision<S>> = (0..nc)
            .map(|c| collision(&f[c * Q..(c + 1) * Q], q[c], &g[c], tau[c], &us[c], beta[c]))
            .collect();
        let (fs, ps) = physical_scales(rho, h, dt);
        let post: Vec<S> = cells.iter().flat_map(|c| c.post).collect();
        let solid_force: Vec<[S; 3]> =
            cells.iter().map(|c| c.drag.solid_reaction_force_density.map(|v| v * fs)).collect();
        let heat: Vec<S> = cells.iter().map(|c| c.drag.relative_work_dissipation * ps).collect();
        Drag {
            post,
            fluid_drag_force_n: cells
                .iter()
                .map(|c| c.drag.fluid_drag_force_density.map(|v| v * fs))
                .collect(),
            solid_nodal_force_n: self.qmap.apply_t_vec(&solid_force),
            shared_nodal_heat_w: caloric.lmap().apply_t(&heat),
            solid_reaction_force_n: solid_force,
            drag_dissipation_w: heat,
            fluid_drag_work_w: cells.iter().map(|c| c.drag.fluid_drag_work * ps).collect(),
            solid_drag_work_w: cells.iter().map(|c| c.drag.solid_drag_work * ps).collect(),
            cells,
        }
    }
}
