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

use crate::surface_gap::GapCellGeometry;
use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::field::MovingLbmConfig;

pub struct GapEnvelope {
    pub normal_axis: usize,
    pub cell_bounds: [[usize; 3]; 2],
    pub tolerance_m: f64,
}

fn cross(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}
fn area(p: &[[f64; 2]]) -> f64 {
    if p.len() < 3 {
        return 0.;
    }
    (1..p.len() - 1)
        .map(|i| cross(p[0], p[i], p[i + 1]))
        .sum::<f64>()
        .abs()
        * 0.5
}
fn overlap(a: [[f64; 2]; 3], b: [[f64; 2]; 3]) -> f64 {
    let sign = if cross(b[0], b[1], b[2]) >= 0. {
        1.
    } else {
        -1.
    };
    let mut polygon = a.to_vec();
    for i in 0..3 {
        let mut clipped = Vec::new();
        if polygon.is_empty() {
            break;
        }
        let left = b[i];
        let right = b[(i + 1) % 3];
        for j in 0..polygon.len() {
            let start = polygon[j];
            let end = polygon[(j + 1) % polygon.len()];
            let s = sign * cross(left, right, start);
            let e = sign * cross(left, right, end);
            if s >= 0. {
                clipped.push(start);
            }
            if (s >= 0.) != (e >= 0.) {
                let t = s / (s - e);
                clipped.push(std::array::from_fn(|k| start[k] + t * (end[k] - start[k])));
            }
        }
        polygon = clipped;
    }
    area(&polygon)
}

impl GapEnvelope {
    pub fn validate(
        &self,
        bulk: &MovingLbmConfig,
        excluded: &[usize],
        reference: &[GapCellGeometry<f64>],
        current: &[GapCellGeometry<f64>],
    ) -> CaeResult<f64> {
        let fail = |s: &str| CaeError::contract(s);
        if self.normal_axis >= 3
            || !self.tolerance_m.is_finite()
            || self.tolerance_m <= 0.
            || !bulk.spacing_m.is_finite()
            || bulk.spacing_m <= 0.
            || bulk.origin_m.iter().any(|v| !v.is_finite())
            || reference.is_empty()
            || current.len() != reference.len()
            || current.len() > 256
            || (0..3).any(|a| {
                self.cell_bounds[0][a] >= self.cell_bounds[1][a]
                    || self.cell_bounds[1][a] > bulk.shape[a]
            })
        {
            return Err(fail("invalid gap envelope"));
        }
        let lo: [f64; 3] = std::array::from_fn(|a| {
            bulk.origin_m[a] + self.cell_bounds[0][a] as f64 * bulk.spacing_m
        });
        let hi: [f64; 3] = std::array::from_fn(|a| {
            bulk.origin_m[a] + self.cell_bounds[1][a] as f64 * bulk.spacing_m
        });
        if self.tolerance_m > bulk.spacing_m * 1e-4 {
            return Err(fail("gap envelope geometry tolerance is too large"));
        }
        if lo.iter().chain(&hi).any(|v| !v.is_finite()) || (0..3).any(|a| hi[a] <= lo[a]) {
            return Err(fail("gap envelope dimensions exceed finite geometry"));
        }
        let count = (0..3)
            .try_fold(1usize, |n, a| {
                n.checked_mul(self.cell_bounds[1][a] - self.cell_bounds[0][a])
            })
            .ok_or_else(|| fail("gap envelope cell count overflow"))?;
        let native_count = bulk
            .shape
            .iter()
            .try_fold(1usize, |n, m| n.checked_mul(*m))
            .ok_or_else(|| fail("bulk cell count overflow"))?;
        if bulk.solid_mask.len() != native_count {
            return Err(fail("bulk mask size does not match its grid"));
        }
        let mut cells = excluded.to_vec();
        cells.sort_unstable();
        if cells.len() != count || cells.windows(2).any(|v| v[0] == v[1]) {
            return Err(fail("excluded cells must equal the gap envelope"));
        }
        for z in self.cell_bounds[0][2]..self.cell_bounds[1][2] {
            for y in self.cell_bounds[0][1]..self.cell_bounds[1][1] {
                for x in self.cell_bounds[0][0]..self.cell_bounds[1][0] {
                    let index = (x * bulk.shape[1] + y) * bulk.shape[2] + z;
                    if cells.binary_search(&index).is_err()
                        || !bulk.solid_mask.get(index).copied().unwrap_or(false)
                    {
                        return Err(fail("gap envelope requires exclusively masked cells"));
                    }
                }
            }
        }
        let axes = [(self.normal_axis + 1) % 3, (self.normal_axis + 2) % 3];
        let mut triangles = Vec::new();
        let mut sum = 0.;
        for (initial, g) in reference.iter().zip(current) {
            if initial.vertices_m.iter().flatten().any(|v| !v.is_finite()) {
                return Err(fail("reference gap geometry must be finite"));
            }
            for i in 0..6 {
                let p = g.vertices_m[i];
                if p.iter().any(|v| !v.is_finite())
                    || (0..3)
                        .any(|a| p[a] < lo[a] - self.tolerance_m || p[a] > hi[a] + self.tolerance_m)
                    || axes
                        .iter()
                        .any(|a| (p[*a] - initial.vertices_m[i][*a]).abs() > self.tolerance_m)
                    || axes
                        .iter()
                        .any(|a| (p[*a] - g.vertices_m[i % 3][*a]).abs() > self.tolerance_m)
                {
                    return Err(fail("moving gap leaves its fixed prismatic envelope"));
                }
            }
            if (0..3)
                .any(|i| g.vertices_m[i + 3][self.normal_axis] <= g.vertices_m[i][self.normal_axis])
            {
                return Err(fail("gap covers cross within the envelope"));
            }
            let triangle =
                std::array::from_fn(|i| [g.vertices_m[i][axes[0]], g.vertices_m[i][axes[1]]]);
            let value = 0.5 * cross(triangle[0], triangle[1], triangle[2]);
            if !value.is_finite() || value <= 0. || !g.volume_m3.is_finite() || g.volume_m3 <= 0. {
                return Err(fail("gap footprint is inverted or degenerate"));
            }
            sum += value;
            triangles.push(triangle);
        }
        let footprint = (hi[axes[0]] - lo[axes[0]]) * (hi[axes[1]] - lo[axes[1]]);
        let area_tolerance =
            self.tolerance_m * ((hi[axes[0]] - lo[axes[0]]) + (hi[axes[1]] - lo[axes[1]])) * 4.;
        if !footprint.is_finite()
            || footprint <= 0.
            || !sum.is_finite()
            || !area_tolerance.is_finite()
        {
            return Err(fail("gap footprint exceeds finite arithmetic"));
        }
        if (sum - footprint).abs() > area_tolerance {
            return Err(fail("gap patches do not cover the envelope footprint"));
        }
        for i in 0..triangles.len() {
            for j in 0..i {
                if overlap(triangles[i], triangles[j])
                    > area_tolerance / (triangles.len() as f64).powi(2)
                {
                    return Err(fail("gap footprints overlap"));
                }
            }
        }
        let volume: f64 = current.iter().map(|g| g.volume_m3).sum();
        if !volume.is_finite() {
            return Err(fail("gap envelope volume exceeds finite arithmetic"));
        }
        Ok(volume)
    }
}
