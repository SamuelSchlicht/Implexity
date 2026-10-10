// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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
