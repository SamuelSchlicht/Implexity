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

use crate::{
    moving_contact::{mapped_pair_law::NodeBinding, native_surface_binding::NativeNodalBinding},
    surface_gap::GapCellGeometry,
};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| a[i] * b[i]).sum()
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}
#[derive(Clone, Copy)]
struct Vertex {
    point: [f64; 3],
    density: f64,
}
type Polyhedron = Vec<Vec<Vertex>>;
#[derive(Clone)]
pub struct NativeGapGeometryOwner {
    nodes: [Vec<NodeBinding>; 2],
    elements: [Vec<[usize; 4]>; 2],
    phase: CsrMatrix,
    iso: f64,
    tolerance_m: f64,
    trace_size: usize,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct NativeGapPartitionReceipt {
    pub solid_volumes_m3: [f64; 2],
    pub expected_skin_volumes_m3: [f64; 2],
    pub gap_volume_m3: f64,
    pub volume_defect_m3: f64,
    pub forbidden_body_volumes_m3: [f64; 2],
}
fn interpolate(a: Vertex, b: Vertex, t: f64) -> Vertex {
    Vertex {
        point: std::array::from_fn(|i| a.point[i] + (b.point[i] - a.point[i]) * t),
        density: a.density + (b.density - a.density) * t,
    }
}
fn clip(
    poly: &Polyhedron,
    value: &impl Fn(Vertex) -> f64,
    normal: [f64; 3],
    tolerance: f64,
) -> CaeResult<Polyhedron> {
    let mut result = Vec::new();
    let mut cap: Vec<Vertex> = Vec::new();
    for face in poly {
        if face.len() < 3 {
            continue;
        }
        let mut out = Vec::new();
        for i in 0..face.len() {
            let a = face[i];
            let b = face[(i + 1) % face.len()];
            let va = value(a);
            let vb = value(b);
            if !va.is_finite() || !vb.is_finite() {
                return Err(fail("nonfinite native clipping plane"));
            }
            if va >= 0. {
                out.push(a);
            }
            if (va >= 0.) != (vb >= 0.) {
                let x = interpolate(a, b, va / (va - vb));
                out.push(x);
                if cap.iter().all(|v| norm(sub(v.point, x.point)) > tolerance) {
                    cap.push(x);
                }
            }
        }
        if out.len() >= 3 {
            result.push(out);
        }
    }
    if cap.len() >= 3 {
        let mut centre = [0.; 3];
        for v in &cap {
            for i in 0..3 {
                centre[i] += v.point[i] / cap.len() as f64;
            }
        }
        let nn = norm(normal);
        if !nn.is_finite() || nn <= 0. {
            return Err(fail("invalid native clipping normal"));
        }
        let n = normal.map(|v| v / nn);
        let axis = if n[0].abs() < 0.8 {
            [1., 0., 0.]
        } else {
            [0., 1., 0.]
        };
        let a = cross(n, axis);
        let a = a.map(|v| v / norm(a));
        let b = cross(n, a);
        cap.sort_by(|u, v| {
            let up = sub(u.point, centre);
            let vp = sub(v.point, centre);
            dot(up, b)
                .atan2(dot(up, a))
                .total_cmp(&dot(vp, b).atan2(dot(vp, a)))
        });
        result.push(cap);
    }
    Ok(result)
}
fn plane(poly: &Polyhedron, normal: [f64; 3], origin: [f64; 3], tol: f64) -> CaeResult<Polyhedron> {
    clip(poly, &|v| dot(normal, sub(v.point, origin)), normal, tol)
}
fn volume(poly: &Polyhedron) -> f64 {
    let count: usize = poly.iter().map(Vec::len).sum();
    if count == 0 {
        return 0.;
    }
    let mut centre = [0.; 3];
    for v in poly.iter().flatten() {
        for i in 0..3 {
            centre[i] += v.point[i] / count as f64;
        }
    }
    poly.iter()
        .map(|f| {
            (1..f.len().saturating_sub(1))
                .map(|i| {
                    dot(
                        sub(f[0].point, centre),
                        cross(sub(f[i].point, centre), sub(f[i + 1].point, centre)),
                    )
                    .abs()
                        / 6.
                })
                .sum::<f64>()
        })
        .sum()
}
fn tetra(vertices: [Vertex; 4]) -> Polyhedron {
    [[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]]
        .map(|f| f.map(|i| vertices[i]).to_vec())
        .to_vec()
}
impl NativeGapGeometryOwner {
    pub fn from_native(
        binding: &NativeNodalBinding,
        iso: f64,
        tolerance_m: f64,
    ) -> CaeResult<Self> {
        Self::from_parts(
            binding.nodes().clone(),
            std::array::from_fn(|b| binding.meshes()[b].mesh.elements.clone()),
            binding.phase().clone(),
            binding.flux_size(),
            iso,
            tolerance_m,
        )
    }
    pub fn from_parts(
        nodes: [Vec<NodeBinding>; 2],
        elements: [Vec<[usize; 4]>; 2],
        phase: CsrMatrix,
        trace_size: usize,
        iso: f64,
        tolerance_m: f64,
    ) -> CaeResult<Self> {
        if !iso.is_finite()
            || iso <= 0.
            || iso >= 1.
            || !tolerance_m.is_finite()
            || tolerance_m <= 0.
            || trace_size == 0
            || phase.nrows() != nodes[0].len() + nodes[1].len()
            || nodes.iter().flatten().any(|n| {
                n.reference_m.iter().any(|v| !v.is_finite())
                    || n.force_flux.iter().any(|i| *i >= trace_size)
            })
            || (0..2).any(|b| {
                elements[b].is_empty()
                    || elements[b]
                        .iter()
                        .any(|e| e.iter().any(|i| *i >= nodes[b].len()))
            })
        {
            return Err(fail("invalid native gap geometric ownership policy"));
        }
        Ok(Self {
            nodes,
            elements,
            phase,
            iso,
            tolerance_m,
            trace_size,
        })
    }
    pub fn validate(
        &self,
        trace: &[f64],
        design: &[f64],
        bounds: [[f64; 3]; 2],
        axis: usize,
        gap: &[GapCellGeometry<f64>],
        absolute_tolerance_m3: f64,
        relative_tolerance: f64,
    ) -> CaeResult<NativeGapPartitionReceipt> {
        if trace.len() != self.trace_size
            || design.len() != self.phase.ncols()
            || axis >= 3
            || gap.is_empty()
            || trace
                .iter()
                .chain(design)
                .chain(bounds.iter().flatten())
                .any(|v| !v.is_finite())
            || (0..3).any(|i| bounds[1][i] <= bounds[0][i])
            || ![absolute_tolerance_m3, relative_tolerance]
                .iter()
                .all(|v| v.is_finite() && *v >= 0.)
        {
            return Err(fail("invalid native geometric partition input"));
        }
        let mut density = vec![0.; self.phase.nrows()];
        for (i, v) in density.iter_mut().enumerate() {
            let (c, w) = self.phase.row(i);
            for (&j, &a) in c.iter().zip(w) {
                *v += design[j] * a;
            }
            if !v.is_finite() || !(0. ..=1.).contains(v) {
                return Err(fail("native geometric partition density domain"));
            }
        }
        let positions: [Vec<[f64; 3]>; 2] = std::array::from_fn(|b| {
            self.nodes[b]
                .iter()
                .map(|n| std::array::from_fn(|a| n.reference_m[a] + trace[n.force_flux[a]]))
                .collect()
        });
        if positions.iter().flatten().flatten().any(|v| !v.is_finite()) {
            return Err(fail("nonfinite deformed native gap geometry"));
        }
        let mut expected = [0.; 2];
        let mut projections = Vec::new();
        let mut axial = [0.; 3];
        axial[axis] = 1.;
        for g in gap {
            let v = g.vertices_m;
            if v.iter().flatten().any(|x| !x.is_finite())
                || (0..3).any(|i| {
                    (0..3).any(|a| a != axis && (v[i][a] - v[i + 3][a]).abs() > self.tolerance_m)
                })
            {
                return Err(fail(
                    "native partition requires corresponding normal-axis skin columns",
                ));
            }
            let cl = cross(sub(v[1], v[0]), sub(v[2], v[0])).map(|x| x * 0.5);
            let cu = cross(sub(v[4], v[3]), sub(v[5], v[3])).map(|x| x * 0.5);
            if cl[axis] <= 0. || cu[axis] <= 0. {
                return Err(fail("native partition skin orientation"));
            }
            expected[0] +=
                cl[axis] * ((v[0][axis] + v[1][axis] + v[2][axis]) / 3. - bounds[0][axis]);
            expected[1] +=
                cu[axis] * (bounds[1][axis] - (v[3][axis] + v[4][axis] + v[5][axis]) / 3.);
            let side: [([f64; 3], [f64; 3]); 3] = std::array::from_fn(|i| {
                let edge = sub(v[(i + 1) % 3], v[i]);
                (cross(axial, edge), v[i])
            });
            projections.push((side, cl, cu, v[0], v[3]));
        }
        let envelope = (0..3).map(|i| bounds[1][i] - bounds[0][i]).product::<f64>();
        let tolerance = absolute_tolerance_m3 + relative_tolerance * envelope;
        if !envelope.is_finite() || !tolerance.is_finite() {
            return Err(fail("native partition volume domain"));
        }
        let mut solid = [0.; 2];
        let mut pieces: [Vec<Polyhedron>; 3] = std::array::from_fn(|_| Vec::new());
        for g in gap {
            let vv = g.vertices_m;
            if vv.iter().any(|v| {
                (0..3).any(|a| {
                    v[a] < bounds[0][a] - self.tolerance_m || v[a] > bounds[1][a] + self.tolerance_m
                })
            }) || (0..3).any(|i| vv[i + 3][axis] <= vv[i][axis])
            {
                return Err(fail("native gap outside ownership envelope"));
            }
            let area = cross(sub(vv[1], vv[0]), sub(vv[2], vv[0]))[axis] * 0.5;
            let exact = area * (0..3).map(|i| vv[i + 3][axis] - vv[i][axis]).sum::<f64>() / 3.;
            if !g.volume_m3.is_finite() || (g.volume_m3 - exact).abs() > tolerance {
                return Err(fail("native gap geometry volume mismatch"));
            }
            pieces[2].push(
                vec![
                    vec![0, 1, 2],
                    vec![3, 4, 5],
                    vec![0, 1, 4, 3],
                    vec![1, 2, 5, 4],
                    vec![2, 0, 3, 5],
                ]
                .iter()
                .map(|ids| {
                    ids.iter()
                        .map(|i| Vertex {
                            point: vv[*i],
                            density: 1.,
                        })
                        .collect()
                })
                .collect(),
            );
        }
        let mut forbidden = [0.; 2];
        for body in 0..2 {
            let offset = if body == 0 { 0 } else { self.nodes[0].len() };
            for element in &self.elements[body] {
                if element.iter().any(|i| *i >= positions[body].len()) {
                    return Err(fail("native partition tetrahedron index"));
                }
                let vertices = element.map(|i| Vertex {
                    point: positions[body][i],
                    density: density[offset + i],
                });
                if vertices.iter().all(|v| v.density < self.iso)
                    || (0..3).any(|a| {
                        vertices.iter().all(|v| v.point[a] < bounds[0][a])
                            || vertices.iter().all(|v| v.point[a] > bounds[1][a])
                    })
                {
                    continue;
                }
                let edges = [
                    sub(vertices[1].point, vertices[0].point),
                    sub(vertices[2].point, vertices[0].point),
                    sub(vertices[3].point, vertices[0].point),
                ];
                let det = dot(edges[0], cross(edges[1], edges[2]));
                let reference = element.map(|i| self.nodes[body][i].reference_m);
                let reference_det = dot(
                    sub(reference[1], reference[0]),
                    cross(
                        sub(reference[2], reference[0]),
                        sub(reference[3], reference[0]),
                    ),
                );
                if !det.is_finite()
                    || !reference_det.is_finite()
                    || det == 0.
                    || reference_det == 0.
                    || det.is_sign_positive() != reference_det.is_sign_positive()
                {
                    return Err(fail("native partition tetrahedron is inverted"));
                }
                let mut poly = tetra(vertices);
                for a in 0..3 {
                    let mut normal = [0.; 3];
                    normal[a] = 1.;
                    poly = plane(&poly, normal, bounds[0], self.tolerance_m)?;
                    normal[a] = -1.;
                    poly = plane(&poly, normal, bounds[1], self.tolerance_m)?;
                }
                if vertices.iter().any(|v| v.density < self.iso) {
                    let dr = [
                        vertices[1].density - vertices[0].density,
                        vertices[2].density - vertices[0].density,
                        vertices[3].density - vertices[0].density,
                    ];
                    let n = std::array::from_fn(|a| {
                        (cross(edges[1], edges[2])[a] * dr[0]
                            + cross(edges[2], edges[0])[a] * dr[1]
                            + cross(edges[0], edges[1])[a] * dr[2])
                            / det
                    });
                    poly = clip(&poly, &|v| v.density - self.iso, n, self.tolerance_m)?;
                }
                let pv = volume(&poly);
                if pv > 0. {
                    pieces[body].push(poly.clone());
                }
                solid[body] += pv;
                for (sides, cl, cu, lower, upper) in &projections {
                    let mut region = poly.clone();
                    for (n, o) in sides {
                        region = plane(&region, *n, *o, self.tolerance_m)?;
                    }
                    let (n, o) = if body == 0 {
                        (*cl, *lower)
                    } else {
                        (cu.map(|v| -v), *upper)
                    };
                    region = plane(&region, n, o, self.tolerance_m)?;
                    forbidden[body] += volume(&region);
                }
            }
        }
        let mut overlap_total = 0.;
        for body_pieces in &mut pieces {
            let aabb = |p: &Polyhedron| -> [[f64; 3]; 2] {
                std::array::from_fn(|s| {
                    std::array::from_fn(|a| {
                        p.iter().flatten().map(|v| v.point[a]).fold(
                            if s == 0 {
                                f64::INFINITY
                            } else {
                                f64::NEG_INFINITY
                            },
                            |x, y| if s == 0 { x.min(y) } else { x.max(y) },
                        )
                    })
                })
            };
            body_pieces.sort_by(|a, b| aabb(a)[0][0].total_cmp(&aabb(b)[0][0]));
            for i in 0..body_pieces.len() {
                let ba = aabb(&body_pieces[i]);
                for j in i + 1..body_pieces.len() {
                    let bb = aabb(&body_pieces[j]);
                    if bb[0][0] >= ba[1][0] {
                        break;
                    }
                    if (0..3).any(|a| ba[1][a] <= bb[0][a] || bb[1][a] <= ba[0][a]) {
                        continue;
                    }
                    let count: usize = body_pieces[j].iter().map(Vec::len).sum();
                    let centre = std::array::from_fn(|a| {
                        body_pieces[j]
                            .iter()
                            .flatten()
                            .map(|v| v.point[a])
                            .sum::<f64>()
                            / count as f64
                    });
                    let mut intersection = body_pieces[i].clone();
                    for face in &body_pieces[j] {
                        let Some(n) = (1..face.len().saturating_sub(1))
                            .map(|k| {
                                cross(
                                    sub(face[k].point, face[0].point),
                                    sub(face[k + 1].point, face[0].point),
                                )
                            })
                            .max_by(|a, b| norm(*a).total_cmp(&norm(*b)))
                            .filter(|n| norm(*n) > 0.)
                        else {
                            continue;
                        };
                        let n = if dot(n, sub(centre, face[0].point)) < 0. {
                            n.map(|v| -v)
                        } else {
                            n
                        };
                        intersection = plane(&intersection, n, face[0].point, self.tolerance_m)?;
                    }
                    let overlap = volume(&intersection);
                    overlap_total += overlap;
                    if !overlap.is_finite()
                        || !overlap_total.is_finite()
                        || overlap_total > tolerance
                    {
                        return Err(fail("native sharp pieces overlap inside gap envelope"));
                    }
                }
            }
        }
        let gap_volume = gap.iter().map(|g| g.volume_m3).sum::<f64>();
        let defect = envelope - solid.iter().sum::<f64>() - gap_volume;
        if !envelope.is_finite()
            || !tolerance.is_finite()
            || !gap_volume.is_finite()
            || !defect.is_finite()
            || defect.abs() > tolerance
            || (0..2).any(|b| {
                !solid[b].is_finite()
                    || !expected[b].is_finite()
                    || expected[b] < 0.
                    || (solid[b] - expected[b]).abs() > tolerance
                    || !forbidden[b].is_finite()
                    || forbidden[b] > tolerance
            })
        {
            return Err(fail(
                "native sharp solid, skin and gap partition is inconsistent",
            ));
        }
        Ok(NativeGapPartitionReceipt {
            solid_volumes_m3: solid,
            expected_skin_volumes_m3: expected,
            gap_volume_m3: gap_volume,
            volume_defect_m3: defect,
            forbidden_body_volumes_m3: forbidden,
        })
    }
}
