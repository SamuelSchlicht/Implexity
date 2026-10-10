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


use implexity_core::CaeError;

use crate::hyperelastic::kinematics::TetMesh;
use crate::util::contract;

#[derive(Debug, Clone, PartialEq)]
pub struct VoxelGrid {
    pub origin_m: [f64; 3],
    pub shape: [usize; 3],
    pub element_size_m: f64,
}

impl VoxelGrid {

    pub fn new(origin_m: [f64; 3], shape: [usize; 3], element_size_m: f64) -> Result<Self, CaeError> {
        if shape.contains(&0) || shape.iter().product::<usize>() > 2_000_000 {
            return contract("the solid reference grid needs 1..2e6 voxels with a positive count per axis");
        }
        if !(element_size_m.is_finite() && element_size_m > 0.0 && origin_m.iter().all(|v| v.is_finite())) {
            return contract("the solid reference grid needs a finite origin and a positive element size");
        }
        Ok(Self { origin_m, shape, element_size_m })
    }

    #[must_use]
    pub fn voxel_count(&self) -> usize {
        self.shape.iter().product()
    }

    #[must_use]
    pub fn node_shape(&self) -> [usize; 3] {
        self.shape.map(|s| s + 1)
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        self.node_shape().iter().product()
    }

    #[must_use]
    pub fn node_index(&self, ijk: [usize; 3]) -> usize {
        let s = self.node_shape();
        (ijk[0] * s[1] + ijk[1]) * s[2] + ijk[2]
    }

    #[must_use]
    pub fn voxel_index(&self, ijk: [usize; 3]) -> usize {
        (ijk[0] * self.shape[1] + ijk[1]) * self.shape[2] + ijk[2]
    }

    #[must_use]
    pub fn node_ijk(&self, n: usize) -> [usize; 3] {
        let s = self.node_shape();
        [n / (s[1] * s[2]), (n / s[2]) % s[1], n % s[2]]
    }

    #[must_use]
    pub fn node_position(&self, n: usize) -> [f64; 3] {
        let ijk = self.node_ijk(n);
        #[allow(clippy::cast_precision_loss)]
        core::array::from_fn(|a| self.origin_m[a] + ijk[a] as f64 * self.element_size_m)
    }

    pub fn nodes_where(&self, select: impl Fn([usize; 3]) -> bool) -> Vec<usize> {
        (0..self.node_count()).filter(|n| select(self.node_ijk(*n))).collect()
    }


    pub fn voxel_nodes(&self, voxels: &[bool]) -> Result<Vec<usize>, CaeError> {
        if voxels.len() != self.voxel_count() {
            return contract("voxel masks need one flag per reference voxel");
        }
        let mut flag = vec![false; self.node_count()];
        for i in 0..self.shape[0] {
            for j in 0..self.shape[1] {
                for k in 0..self.shape[2] {
                    if voxels[self.voxel_index([i, j, k])] {
                        for c in 0..8 {
                            flag[self.node_index([i + (c >> 2), j + ((c >> 1) & 1), k + (c & 1)])] = true;
                        }
                    }
                }
            }
        }
        Ok((0..flag.len()).filter(|n| flag[*n]).collect())
    }


    pub fn fixed_mask(
        &self,
        nodes: &[usize],
        components: [bool; 3],
        plane_strain: bool,
    ) -> Result<Vec<bool>, CaeError> {
        if plane_strain && self.shape[2] != 1 {
            return contract("plane strain requires exactly one voxel layer along z");
        }
        let mut fixed = vec![false; 3 * self.node_count()];
        for n in nodes {
            if *n >= self.node_count() {
                return contract("support nodes must be grid nodes");
            }
            for i in 0..3 {
                fixed[3 * n + i] |= components[i];
            }
        }
        if plane_strain {
            for n in 0..self.node_count() {
                fixed[3 * n + 2] = true;
            }
        }
        Ok(fixed)
    }
}

#[derive(Debug, Clone)]
pub struct KuhnMesh {
    pub mesh: TetMesh,
    pub owner: Vec<usize>,
}


pub fn kuhn_tetrahedra(grid: &VoxelGrid) -> Result<KuhnMesh, CaeError> {
    let m = crate::solid_history::mesh(grid.shape)?;
    #[allow(clippy::cast_precision_loss)]
    let points: Vec<[f64; 3]> = m
        .ijk
        .iter()
        .map(|p| core::array::from_fn(|a| grid.origin_m[a] + p[a] as f64 * grid.element_size_m))
        .collect();
    let mesh = TetMesh::new(points, m.tets)?;
    Ok(KuhnMesh { mesh, owner: m.owners })
}

#[derive(Debug, Clone)]
pub struct VoxelDesignMap {
    owner: Vec<usize>,
    voxels: usize,
}

impl VoxelDesignMap {
    #[must_use]
    pub fn new(kuhn: &KuhnMesh, voxels: usize) -> Self {
        Self { owner: kuhn.owner.clone(), voxels }
    }

    #[must_use]
    pub fn voxels(&self) -> usize {
        self.voxels
    }

    #[must_use]
    pub fn owner(&self) -> &[usize] {
        &self.owner
    }

    #[must_use]
    pub fn elements(&self) -> usize {
        self.owner.len()
    }


    pub fn forward(&self, voxel: &[f64]) -> Result<Vec<f64>, CaeError> {
        if voxel.len() != self.voxels {
            return contract("voxel design fields need one value per reference voxel");
        }
        Ok(self.owner.iter().map(|v| voxel[*v]).collect())
    }


    pub fn pullback(&self, element: &[f64]) -> Result<Vec<f64>, CaeError> {
        if element.len() != self.owner.len() {
            return contract("element cotangents need one value per tetrahedron");
        }
        let mut out = vec![0.0; self.voxels];
        for (e, v) in self.owner.iter().enumerate() {
            out[*v] += element[e];
        }
        Ok(out)
    }
}
