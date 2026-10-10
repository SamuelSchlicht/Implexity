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



use std::collections::HashMap;
use std::path::Path;

use implexity_mesh::formats::{Triangle32, read_stl_bytes};

use super::{SceneError, add, cross, dot, length, scale, sub};

#[derive(Clone, Debug)]
pub struct SceneMesh {
    pub positions: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    pub corner_normals: Vec<[[f32; 3]; 3]>,
    pub bounds: [[f64; 3]; 2],
}

impl SceneMesh {
    #[must_use]
    pub fn len(&self) -> usize {
        self.triangles.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    #[must_use]
    pub fn corners(&self, t: usize) -> [[f64; 3]; 3] {
        self.triangles[t].map(|i| self.positions[i as usize].map(f64::from))
    }
}



pub fn load_stl(path: &Path, crease_deg: f64) -> Result<SceneMesh, SceneError> {
    let blob = std::fs::read(path)
        .map_err(|e| SceneError::Invalid(format!("cannot read STL {}: {e}", path.display())))?;
    let (tris, _header) = read_stl_bytes(&blob)
        .map_err(|e| SceneError::Invalid(format!("{} is not a binary STL: {e}", path.display())))?;
    Ok(weld(&tris, crease_deg))
}

#[must_use]
pub fn weld(tris: &[Triangle32], crease_deg: f64) -> SceneMesh {
    let mut index: HashMap<[u32; 3], u32> = HashMap::with_capacity(tris.len() / 2 + 8);
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(tris.len() / 2 + 8);
    let mut triangles: Vec<[u32; 3]> = Vec::with_capacity(tris.len());
    for t in tris {
        let ids = t.map(|p| {

            let key = p.map(|c| if c == 0.0 { 0.0_f32.to_bits() } else { c.to_bits() });
            *index.entry(key).or_insert_with(|| {
                positions.push(p);
                u32::try_from(positions.len() - 1).unwrap_or(u32::MAX)
            })
        });
        if ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2] {
            triangles.push(ids);
        }
    }
    let mut bounds = [[f64::INFINITY; 3], [f64::NEG_INFINITY; 3]];
    for p in &positions {
        for a in 0..3 {
            bounds[0][a] = bounds[0][a].min(f64::from(p[a]));
            bounds[1][a] = bounds[1][a].max(f64::from(p[a]));
        }
    }
    let face: Vec<[f64; 3]> = triangles
        .iter()
        .map(|t| {
            let [a, b, c] = t.map(|i| positions[i as usize].map(f64::from));
            cross(sub(b, a), sub(c, a))
        })
        .collect();

    let mut start = vec![0_usize; positions.len() + 1];
    for t in &triangles {
        for &v in t {
            start[v as usize + 1] += 1;
        }
    }
    for i in 0..positions.len() {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut incident = vec![0_u32; start[positions.len()]];
    for (k, t) in triangles.iter().enumerate() {
        for &v in t {
            incident[fill[v as usize]] = u32::try_from(k).unwrap_or(u32::MAX);
            fill[v as usize] += 1;
        }
    }
    let cos_crease = crease_deg.to_radians().cos();
    let unit = |v: [f64; 3]| {
        let l = length(v);
        if l > 0.0 { scale(v, 1.0 / l) } else { [0.0, 0.0, 0.0] }
    };
    let corner_normals = triangles
        .iter()
        .enumerate()
        .map(|(k, t)| {
            let own = unit(face[k]);
            t.map(|v| {
                let mut n = [0.0; 3];
                for &g in &incident[start[v as usize]..start[v as usize + 1]] {
                    let fg = face[g as usize];
                    if dot(own, unit(fg)) >= cos_crease {
                        n = add(n, fg);
                    }
                }
                let n = unit(n);
                let n = if length(n) > 0.0 { n } else { own };
                n.map(|c| c as f32)
            })
        })
        .collect();
    SceneMesh { positions, triangles, corner_normals, bounds }
}
