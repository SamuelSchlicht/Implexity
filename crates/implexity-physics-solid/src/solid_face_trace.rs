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



use std::collections::{BTreeSet, HashMap};

use implexity_core::CaeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Lo,
    Hi,
}

impl Side {
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "lo" => Some(Self::Lo),
            "hi" => Some(Self::Hi),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SolidFaceTrace {
    pub grid: [usize; 3],
    ijk: Vec<[usize; 3]>,
    tets: Vec<[usize; 4]>,
    order: Vec<usize>,
    offsets: Vec<usize>,
}

fn contract(message: &str) -> CaeError {
    CaeError::contract(message)
}

impl SolidFaceTrace {

    pub fn new(
        grid: [usize; 3],
        ijk: &[[usize; 3]],
        tets: &[[usize; 4]],
        owners: &[usize],
    ) -> Result<Self, CaeError> {
        if owners.len() != tets.len() {
            return Err(contract("solid face trace requires native tetrahedra and cell ownership"));
        }
        let nc: usize = grid.iter().product();
        if owners.iter().any(|o| *o >= nc) || tets.iter().flatten().any(|v| *v >= ijk.len()) {
            return Err(contract("solid face trace contains out-of-range mesh indices"));
        }
        let mut order: Vec<usize> = (0..owners.len()).collect();
        order.sort_by_key(|i| owners[*i]);
        let mut offsets = vec![0usize; nc + 1];
        for o in owners {
            offsets[o + 1] += 1;
        }
        for c in 0..nc {
            offsets[c + 1] += offsets[c];
        }
        Ok(Self { grid, ijk: ijk.to_vec(), tets: tets.to_vec(), order, offsets })
    }


    pub fn triangles(
        &self,
        cell: [usize; 3],
        axis: usize,
        side: Side,
        nodes: [usize; 4],
    ) -> Result<[[usize; 3]; 2], CaeError> {
        let distinct: BTreeSet<usize> = nodes.iter().copied().collect();
        if axis > 2
            || (0..3).any(|a| cell[a] >= self.grid[a])
            || distinct.len() != 4
            || nodes.iter().any(|n| *n >= self.ijk.len())
        {
            return Err(contract("invalid Cartesian solid face trace request"));
        }
        let owner = (cell[0] * self.grid[1] + cell[1]) * self.grid[2] + cell[2];
        let plane = cell[axis] + usize::from(side == Side::Hi);
        let mut faces: Vec<Vec<usize>> = Vec::new();
        for &t in &self.order[self.offsets[owner]..self.offsets[owner + 1]] {
            let tet = self.tets[t];
            let face: Vec<usize> = tet.iter().copied().filter(|v| self.ijk[*v][axis] == plane).collect();
            if face.len() == 3 {
                faces.push(face);
            }
        }
        let lookup: HashMap<usize, usize> = nodes.iter().enumerate().map(|(i, v)| (*v, i)).collect();
        let covered: BTreeSet<usize> = faces.iter().flatten().copied().collect();
        let unique: BTreeSet<Vec<usize>> = faces
            .iter()
            .map(|f| {
                let mut s = f.clone();
                s.sort_unstable();
                s
            })
            .collect();
        if faces.len() != 2
            || faces.iter().flatten().any(|v| !lookup.contains_key(v))
            || covered != distinct
            || unique.len() != 2
        {
            return Err(contract("Cartesian interface must match two actual solid boundary triangles"));
        }

        for face in &faces {
            let p = |k: usize| self.ijk[face[k]].map(|c| c as f64);
            let (a, b, c) = (p(0), p(1), p(2));
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let cross = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            let doubled = (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt();
            #[allow(clippy::float_cmp)]
            if doubled != 1.0 {
                return Err(contract("solid face quadrature requires unit Cartesian reference triangles"));
            }
        }
        Ok([0, 1].map(|i| [0, 1, 2].map(|k| lookup[&faces[i][k]])))
    }


    pub fn quadrature(
        &self,
        cell: [usize; 3],
        axis: usize,
        side: Side,
        nodes: [usize; 4],
    ) -> Result<[[f64; 4]; 6], CaeError> {
        let triangles = self.triangles(cell, axis, side, nodes)?;
        let mut weights = [[0.0; 4]; 6];
        for (i, triangle) in triangles.iter().enumerate() {
            for r in 0..3 {
                for (c, node) in triangle.iter().enumerate() {
                    weights[3 * i + r][*node] = if r == c { 2.0 / 3.0 } else { 1.0 / 6.0 };
                }
            }
        }
        Ok(weights)
    }


    pub fn nodal_weights(
        &self,
        cell: [usize; 3],
        axis: usize,
        side: Side,
        nodes: [usize; 4],
    ) -> Result<[f64; 4], CaeError> {
        let triangles = self.triangles(cell, axis, side, nodes)?;
        let mut weights = [0.0; 4];
        for v in triangles.iter().flatten() {
            weights[*v] += 1.0 / 6.0;
        }
        Ok(weights)
    }
}

