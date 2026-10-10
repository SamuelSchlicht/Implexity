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

use super::voxel::VoxelGrid;
use implexity_core::{CaeError, CaeResult};
#[derive(Clone, Copy, Debug)]
pub struct LinearPressureProfile {
    pub upstream_pa: f64,
    pub start_m: f64,
    pub end_m: f64,
}
impl LinearPressureProfile {
    pub fn at(&self, x: f64) -> f64 {
        if x <= self.start_m {
            self.upstream_pa
        } else if x >= self.end_m {
            0.0
        } else {
            self.upstream_pa * (self.end_m - x) / (self.end_m - self.start_m)
        }
    }
}
fn occupied_neighbour(
    grid: &VoxelGrid,
    density: &[f64],
    cutoff: f64,
    ijk: [usize; 3],
    axis: usize,
    up: bool,
) -> Option<bool> {
    let mut nb = ijk;
    if up {
        nb[axis] += 1;
        if nb[axis] >= grid.shape[axis] {
            return None;
        }
    } else {
        nb[axis] = nb[axis].checked_sub(1)?;
    }
    Some(density[grid.voxel_index(nb)] > cutoff)
}
#[allow(clippy::too_many_arguments)]
pub fn reference_pressure_load(
    grid: &VoxelGrid,
    density: &[f64],
    supported: &[bool],
    cutoff: f64,
    axes: &[usize],
    skip_supported_outer_axes: &[usize],
    profile: LinearPressureProfile,
) -> CaeResult<Vec<f64>> {
    if density.len() != grid.shape.iter().product::<usize>()
        || supported.len() != density.len()
        || !cutoff.is_finite()
        || axes.iter().chain(skip_supported_outer_axes).any(|&a| a >= 3)
        || !profile.upstream_pa.is_finite()
        || !profile.start_m.is_finite()
        || !profile.end_m.is_finite()
        || profile.end_m <= profile.start_m
    {
        return Err(CaeError::contract("invalid reference voxel pressure load"));
    }
    let s = grid.shape;
    let h = grid.element_size_m;
    let o = grid.origin_m;
    let mut force = vec![0.0; 3 * grid.node_count()];
    for i in 0..s[0] {
        for j in 0..s[1] {
            for k in 0..s[2] {
                let v = grid.voxel_index([i, j, k]);
                if density[v] <= cutoff {
                    continue;
                }
                for &axis in axes {
                    for up in [false, true] {
                        let neighbour = occupied_neighbour(grid, density, cutoff, [i, j, k], axis, up);
                        if neighbour == Some(true) {
                            continue;
                        }
                        if skip_supported_outer_axes.contains(&axis) && neighbour.is_none() && supported[v] {
                            continue;
                        }
                        let sign = if up { 1.0 } else { -1.0 };
                        let index = [i, j, k];
                        let mut centre: [f64; 3] =
                            core::array::from_fn(|a| o[a] + (index[a] as f64 + 0.5) * h);
                        centre[axis] += 0.5 * sign * h;
                        let p = profile.at(centre[0]);
                        if p == 0.0 {
                            continue;
                        }
                        let plane = if up { index[axis] + 1 } else { index[axis] };
                        let tangents: Vec<_> = (0..3).filter(|&a| a != axis).collect();
                        for (du, dw) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                            let mut node = index;
                            node[axis] = plane;
                            node[tangents[0]] += du;
                            node[tangents[1]] += dw;
                            let n = grid.node_index(node);
                            force[3 * n + axis] -= sign * p * h * h / 4.0;
                        }
                    }
                }
            }
        }
    }
    Ok(force)
}
