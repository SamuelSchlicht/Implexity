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



use crate::cast::{f32_of, i64_of, idx, trunc_i64};

const CASES: [&[(usize, usize)]; 16] = [
    &[],
    &[(3, 0)],
    &[(0, 1)],
    &[(3, 1)],
    &[(1, 2)],
    &[(3, 0), (1, 2)],
    &[(0, 2)],
    &[(3, 2)],
    &[(2, 3)],
    &[(2, 0)],
    &[(2, 3), (0, 1)],
    &[(2, 1)],
    &[(1, 3)],
    &[(1, 0)],
    &[(0, 3)],
    &[],
];

pub type Segment = [[f64; 2]; 2];

#[must_use]
pub fn marching_squares_extractor(field: &[f64], shape: [usize; 2], level: f64) -> Vec<Segment> {
    marching_squares(field, shape[0], shape[1], level)
}

#[must_use]
pub fn marching_squares(field: &[f64], nx: usize, ny: usize, level: f64) -> Vec<Segment> {
    if nx < 2 || ny < 2 {
        return Vec::new();
    }
    let at = |i: usize, j: usize| field[i * ny + j];
    let interp = |a: f64, b: f64| {
        let d = b - a;
        let t = if d.abs() < 1e-300 { 0.5 } else { (level - a) / if d == 0.0 { 1.0 } else { d } };
        crate::numeric::clip(t, 0.0, 1.0)
    };
    let cells = (nx - 1) * (ny - 1);
    let mut codes = Vec::with_capacity(cells);
    let mut points = Vec::with_capacity(cells);
    for i in 0..nx - 1 {
        for j in 0..ny - 1 {
            let (v00, v10, v11, v01) = (at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1));
            let b = usize::from(v00 > level)
                | (usize::from(v10 > level) << 1)
                | (usize::from(v11 > level) << 2)
                | (usize::from(v01 > level) << 3);
            codes.push(b);
            let (fi, fj) = (i as f64, j as f64);
            let t0 = interp(v00, v10);
            let t1 = interp(v10, v11);
            let t2 = interp(v01, v11);
            let t3 = interp(v00, v01);
            points.push([
                [fi + t0, fj + 0.0 * t0],
                [fi + 1.0 + 0.0 * t1, fj + t1],
                [fi + t2, fj + 1.0 + 0.0 * t2],
                [fi + 0.0 * t3, fj + t3],
            ]);
        }
    }
    let mut out = Vec::new();
    for (code, pairs) in CASES.iter().enumerate() {
        if pairs.is_empty() {
            continue;
        }
        let sel: Vec<usize> = (0..cells).filter(|&c| codes[c] == code).collect();
        if sel.is_empty() {
            continue;
        }
        for &(e0, e1) in *pairs {
            out.extend(sel.iter().map(|&c| [points[c][e0], points[c][e1]]));
        }
    }
    out
}


#[must_use]
pub fn chain(segs: &[Segment], tol: f64) -> Vec<Vec<[f64; 2]>> {
    let m = segs.len();
    if m == 0 {
        return Vec::new();
    }
    let keys: Vec<i64> = segs
        .iter()
        .flat_map(|s| s.iter())
        .map(|p| {
            let q0 = trunc_i64((p[0] / tol).round_ties_even());
            let q1 = trunc_i64((p[1] / tol).round_ties_even());
            q0.wrapping_mul(0x4000_0000).wrapping_add(q1)
        })
        .collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    sorted.dedup();
    let inv: Vec<usize> = keys.iter().map(|k| sorted.partition_point(|s| s < k)).collect();
    let n_nodes = sorted.len();
    let mut nbr = vec![[-1i64; 2]; n_nodes];
    let mut slot = vec![0usize; n_nodes];
    for si in 0..m {
        for e in 0..2 {
            let nd = inv[2 * si + e];
            let k = slot[nd];
            if k < 2 {
                nbr[nd][k] = i64_of(2 * si + e);
                slot[nd] = k + 1;
            }
        }
    }
    let pts: Vec<[f64; 2]> = segs.iter().flat_map(|s| s.iter().copied()).collect();
    let mut used = vec![false; m];
    let mut polys = Vec::new();
    for start in 0..m {
        if used[start] {
            continue;
        }
        used[start] = true;
        let mut forward_part = vec![pts[2 * start], pts[2 * start + 1]];
        let mut backward_part: Vec<[f64; 2]> = Vec::new();
        for forward in [true, false] {
            let mut node = if forward { inv[2 * start + 1] } else { inv[2 * start] };
            loop {
                let mut nxt = -1i64;
                for &code in &nbr[node] {
                    if code < 0 {
                        continue;
                    }
                    if !used[idx(code >> 1)] {
                        nxt = code;
                        break;
                    }
                }
                if nxt < 0 {
                    break;
                }
                let (si, e) = (idx(nxt >> 1), idx(nxt & 1));
                used[si] = true;
                let other = 1 - e;
                let p = pts[2 * si + other];
                if forward {
                    forward_part.push(p);
                } else {
                    backward_part.push(p);
                }
                node = inv[2 * si + other];
            }
        }
        backward_part.reverse();
        backward_part.extend(forward_part);
        polys.push(backward_part);
    }
    polys
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionFrame {
    pub origin: [f64; 3],
    pub h: f64,
    pub u_axis: [f64; 3],
    pub v_axis: [f64; 3],
}


#[must_use]
pub fn section_polylines(
    field: &[f64],
    nx: usize,
    ny: usize,
    frame: &SectionFrame,
    level: f64,
    chain_them: bool,
) -> Vec<Vec<[f32; 3]>> {
    let SectionFrame { origin, h, u_axis, v_axis } = *frame;
    let segs = marching_squares(field, nx, ny, level);
    if segs.is_empty() {
        return Vec::new();
    }
    let polys: Vec<Vec<[f64; 2]>> =
        if chain_them { chain(&segs, 1e-6) } else { segs.iter().map(|s| s.to_vec()).collect() };
    polys
        .iter()
        .map(|p| {
            p.iter()
                .map(|q| {
                    let mut w = [0f32; 3];
                    for a in 0..3 {
                        let x = origin[a] + q[0] * h * u_axis[a] + q[1] * h * v_axis[a];
                        w[a] = f32_of(x * 1000.0);
                    }
                    w
                })
                .collect()
        })
        .collect()
}

