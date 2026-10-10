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




use crate::error::{GResult, GeometryError};
use crate::eval::SAFE_EPS;

pub type Extractor<'a> = &'a dyn Fn(&[f64], [usize; 2], f64) -> Vec<[[f64; 2]; 2]>;

#[must_use]
pub fn count_segments(field: &[f64], shape: [usize; 2], level: f64, extract: Extractor<'_>) -> usize {
    extract(field, shape, level).len()
}

#[derive(Clone, Debug)]
pub struct Contour {
    pub points: Vec<[[f64; 2]; 2]>,
    pub weight: Vec<f64>,
    field: Vec<f64>,
    shape: [usize; 2],
    origin: [f64; 2],
    spacing: [f64; 2],
    grad: [Vec<f64>; 2],
}

fn gradient(f: &[f64], shape: [usize; 2], axis: usize) -> Vec<f64> {
    let ny = shape[1];
    let st = if axis == 0 { ny } else { 1 };
    let n = shape[axis];
    (0..f.len())
        .map(|c| {
            let p = if axis == 0 { c / ny } else { c % ny };
            if n < 2 {
                0.0
            } else if p == 0 {
                f[c + st] - f[c]
            } else if p + 1 == n {
                f[c] - f[c - st]
            } else {
                (f[c + st] - f[c - st]) / 2.0
            }
        })
        .collect()
}

fn bilinear(shape: [usize; 2], p: [f64; 2]) -> [(usize, f64); 4] {
    let clip = |i: f64, n: usize| -> usize {
        #[allow(clippy::cast_precision_loss, clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let v = i.clamp(0.0, n as f64 - 1.0) as usize;
        v
    };
    let (x0, y0) = (p[0].floor(), p[1].floor());
    let (fx, fy) = (p[0] - x0, p[1] - y0);
    let (i0, i1) = (clip(x0, shape[0]), clip(x0 + 1.0, shape[0]));
    let (j0, j1) = (clip(y0, shape[1]), clip(y0 + 1.0, shape[1]));
    let at = |i: usize, j: usize| i * shape[1] + j;
    [
        (at(i0, j0), (1.0 - fx) * (1.0 - fy)),
        (at(i0, j1), (1.0 - fx) * fy),
        (at(i1, j0), fx * (1.0 - fy)),
        (at(i1, j1), fx * fy),
    ]
}

fn sample(f: &[f64], shape: [usize; 2], p: [f64; 2]) -> f64 {
    bilinear(shape, p).iter().map(|(i, w)| f[*i] * w).sum()
}


pub fn contour_from_sdf(
    field: &[f64],
    shape: [usize; 2],
    level: f64,
    max_segments: usize,
    origin: [f64; 2],
    spacing: [f64; 2],
    extract: Extractor<'_>,
) -> GResult<Contour> {
    if max_segments < 1 {
        return Err(GeometryError::Value("max_segments must be at least 1".into()));
    }
    if spacing.iter().copied().fold(f64::INFINITY, f64::min) <= 0.0 {
        return Err(GeometryError::Value(
            "origin and spacing must be 2-vectors with positive spacing".into(),
        ));
    }
    let segs = extract(field, shape, level);
    if segs.len() > max_segments {
        return Err(GeometryError::Value(format!(
            "contour_from_sdf: the field has {} segments but max_segments is {max_segments}; size it with count_segments",
            segs.len()
        )));
    }
    let mut points = vec![[[0.0; 2]; 2]; max_segments];
    let mut weight = vec![0.0; max_segments];
    for (k, s) in segs.iter().enumerate() {
        points[k] = s.map(|p| [origin[0] + p[0] * spacing[0], origin[1] + p[1] * spacing[1]]);
        weight[k] = 1.0;
    }
    let grad = [gradient(field, shape, 0), gradient(field, shape, 1)];
    Ok(Contour { points, weight, field: field.to_vec(), shape, origin, spacing, grad })
}

impl Contour {
    fn idx(&self, p: [f64; 2]) -> [f64; 2] {
        [(p[0] - self.origin[0]) / self.spacing[0], (p[1] - self.origin[1]) / self.spacing[1]]
    }

    fn normal_terms(&self, p: [f64; 2]) -> (f64, f64, f64) {
        let i = self.idx(p);
        let gx = sample(&self.grad[0], self.shape, i) / self.spacing[0];
        let gy = sample(&self.grad[1], self.shape, i) / self.spacing[1];
        (gx, gy, gx * gx + gy * gy + SAFE_EPS)
    }

    #[must_use]
    pub fn jvp(&self, d_field: &[f64], d_level: f64) -> Vec<[[f64; 2]; 2]> {
        self.points
            .iter()
            .zip(&self.weight)
            .map(|(seg, w)| {
                seg.map(|p| {
                    let (gx, gy, n2) = self.normal_terms(p);
                    let dphi = sample(d_field, self.shape, self.idx(p)) - d_level;
                    let scale = -(dphi / n2) * w;
                    [scale * gx, scale * gy]
                })
            })
            .collect()
    }

    #[must_use]
    pub fn vjp(&self, d_points: &[[[f64; 2]; 2]]) -> (Vec<f64>, f64) {
        let mut df = vec![0.0; self.field.len()];
        let mut dl = 0.0;
        for ((seg, w), bar) in self.points.iter().zip(&self.weight).zip(d_points) {
            for (p, b) in seg.iter().zip(bar) {
                let (gx, gy, n2) = self.normal_terms(*p);
                let s = -(w / n2) * (b[0] * gx + b[1] * gy);
                for (i, wt) in bilinear(self.shape, self.idx(*p)) {
                    df[i] += s * wt;
                }
                dl -= s;
            }
        }
        (df, dl)
    }
}

#[must_use]
pub fn segment_lengths(points: &[[[f64; 2]; 2]], weight: &[f64]) -> Vec<f64> {
    points
        .iter()
        .zip(weight)
        .map(|(s, w)| {
            let d = [s[1][0] - s[0][0], s[1][1] - s[0][1]];
            (d[0] * d[0] + d[1] * d[1] + SAFE_EPS).sqrt() * w
        })
        .collect()
}

