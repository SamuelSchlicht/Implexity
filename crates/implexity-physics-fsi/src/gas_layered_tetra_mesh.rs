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

use crate::gas_transient_kinetic::TransientGasScalar;
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct LayeredGapTriangle<S> {
    pub paired_vertex_ids: [usize; 3],
    pub start_m: [[S; 3]; 6],
    pub end_m: [[S; 3]; 6],
}
#[derive(Clone, Debug)]
pub struct LayeredGasNode<S> {
    pub paired_vertex: usize,
    pub layer: usize,
    pub start_m: [S; 3],
    pub end_m: [S; 3],
}
#[derive(Clone, Debug)]
pub struct LayeredGasTetrahedron<S> {
    pub chart: usize,
    pub layer: usize,
    pub local_tetra: usize,
    pub nodes: [usize; 4],
    pub previous_volume_m3: S,
    pub volume_m3: S,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayeredGasBoundary {
    Interior { right: usize },
    LowerCover { chart: usize },
    UpperCover { chart: usize },
    Side { chart: usize, edge: usize },
}
#[derive(Clone, Debug)]
pub struct LayeredGasFacePoint<S> {
    pub left: usize,
    pub boundary: LayeredGasBoundary,
    pub nodes: [usize; 3],
    pub node_weights: [f64; 3],
    pub time_fraction: f64,
    pub position_m: [S; 3],
    pub area_m2: S,
    pub normal: [S; 3],
    pub velocity_m_s: [S; 3],
}
#[derive(Clone, Debug)]
pub struct LayeredGasMesh<S> {
    pub nodes: Vec<LayeredGasNode<S>>,
    pub tetrahedra: Vec<LayeredGasTetrahedron<S>>,
    pub faces: Vec<LayeredGasFacePoint<S>>,
    pub maximum_geometric_defect_m3: f64,
    pub maximum_metric_defect_m2: f64,
}
fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
fn sub<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    std::array::from_fn(|k| a[k] - b[k])
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    (0..3).map(|k| a[k] * b[k]).fold(S::zero(), |a, b| a + b)
}
fn cross<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn determinant<S: Scalar>(a: [S; 3], b: [S; 3], c: [S; 3]) -> S {
    dot(cross(a, b), c)
}
fn volume<S: Scalar>(p: [[S; 3]; 4]) -> S {
    determinant(sub(p[1], p[0]), sub(p[2], p[0]), sub(p[3], p[0])) / 6.
}
fn equal<S: TransientGasScalar>(a: [S; 3], b: [S; 3], tol: f64) -> bool {
    (0..3).all(|k| {
        let d = a[k] - b[k];
        d.finite_first_order() && d.value().abs() <= tol && d.maximum_direction() <= tol
    })
}
fn polynomial(p: [f64; 4], t: f64) -> f64 {
    ((p[3] * t + p[2]) * t + p[1]) * t + p[0]
}
fn sorted(mut x: [usize; 3]) -> [usize; 3] {
    x.sort_unstable();
    x
}
fn certify_path<S: TransientGasScalar>(
    start: [[S; 3]; 4],
    end: [[S; 3]; 4],
    closed_start: bool,
) -> CaeResult<()> {
    let a = sub(start[1], start[0]);
    let b = sub(start[2], start[0]);
    let c = sub(start[3], start[0]);
    let da = sub(sub(end[1], end[0]), a);
    let db = sub(sub(end[2], end[0]), b);
    let dc = sub(sub(end[3], end[0]), c);
    let p = [
        if closed_start {
            S::zero()
        } else {
            determinant(a, b, c)
        },
        determinant(da, b, c) + determinant(a, db, c) + determinant(a, b, dc),
        determinant(da, db, c) + determinant(da, b, dc) + determinant(a, db, dc),
        determinant(da, db, dc),
    ]
    .map(|v| v.value() / 6.);
    if p.iter().any(|v| !v.is_finite()) {
        return Err(fail("layered gas volume polynomial overflow"));
    }
    let mut checks = vec![0., 1.];
    let a = 3. * p[3];
    let b = 2. * p[2];
    let c = p[1];
    if a == 0. {
        if b != 0. {
            checks.push(-c / b);
        }
    } else {
        let discriminant = b * b - 4. * a * c;
        if discriminant >= 0. {
            let q = -0.5
                * (b + if b >= 0. {
                    discriminant.sqrt()
                } else {
                    -discriminant.sqrt()
                });
            if q != 0. {
                checks.push(q / a);
                checks.push(c / q);
            } else {
                checks.push(-b / (2. * a));
            }
        }
    }
    if checks
        .into_iter()
        .filter(|t| *t >= 0. && *t <= 1.)
        .any(|t| polynomial(p, t) < 0.)
    {
        return Err(fail("layered gas tetrahedron inverts on the trace path"));
    }
    Ok(())
}

fn certified_zero_volume<S: TransientGasScalar>(points: [[S; 3]; 4]) -> CaeResult<S> {
    let a = sub(points[1], points[0]);
    let b = sub(points[2], points[0]);
    let c = sub(points[3], points[0]);
    let factors = [
        [a[0], b[1], c[2]],
        [a[0], b[2], c[1]],
        [a[1], b[2], c[0]],
        [a[1], b[0], c[2]],
        [a[2], b[0], c[1]],
        [a[2], b[1], c[0]],
    ];
    let primitive_scale = factors
        .iter()
        .map(|f| f[0].value().abs() * f[1].value().abs() * f[2].value().abs())
        .sum::<f64>()
        / 6.;
    let raw = volume(points);
    let gamma = 32. * f64::EPSILON / (1. - 32. * f64::EPSILON);
    if !raw.finite_first_order() || raw.value().abs() > gamma * primitive_scale {
        return Err(fail("certified closed chart has nonzero actual volume"));
    }
    let directions = raw
        .geometry_first_order_directions()
        .ok_or_else(|| fail("certified closed chart scalar has no direction certificate"))?;
    for (direction, &actual) in directions.iter().enumerate() {
        let mut scale = 0.;
        for f in &factors {
            for k in 0..3 {
                let jet = f[k].geometry_first_order_directions().ok_or_else(|| {
                    fail("certified closed chart factor has no direction certificate")
                })?;
                let mut term = jet[direction].abs();
                for j in 0..3 {
                    if j != k {
                        term *= f[j].value().abs();
                    }
                }
                scale += term / 6.;
            }
        }
        if actual.abs() > gamma * scale {
            return Err(fail(
                "certified closed chart has nonzero actual volume direction",
            ));
        }
    }
    Ok(S::zero())
}

pub fn build<S: TransientGasScalar>(
    triangles: &[LayeredGapTriangle<S>],
    layers: usize,
    step_s: S,
    length_tolerance_m: f64,
) -> CaeResult<LayeredGasMesh<S>> {
    build_selected(triangles, layers, step_s, length_tolerance_m, &[], &[])
}
pub(crate) fn build_closed_release<S: TransientGasScalar>(
    triangles: &[LayeredGapTriangle<S>],
    layers: usize,
    step_s: S,
    length_tolerance_m: f64,
    certificate: &crate::native_closed_release::NativeClosedReleaseCertificate,
    initial_endpoint: bool,
) -> CaeResult<LayeredGasMesh<S>> {
    let start = certificate.closed_start_charts();
    let end = if initial_endpoint {
        start.to_vec()
    } else {
        start
            .iter()
            .copied()
            .filter(|&chart| {
                (0..3).all(|corner| {
                    certificate
                        .closed_endpoint_corners()
                        .contains(&(chart, corner))
                })
            })
            .collect()
    };
    build_selected(triangles, layers, step_s, length_tolerance_m, start, &end)
}
pub(crate) fn build_closed_witness<S: TransientGasScalar>(
    triangles: &[LayeredGapTriangle<S>],
    layers: usize,
    step_s: S,
    length_tolerance_m: f64,
    witness: &crate::native_closed_release::NativeClosedGeometryWitness,
    initial_endpoint: bool,
) -> CaeResult<LayeredGasMesh<S>> {
    let end = if initial_endpoint {
        witness.closed_start_charts()
    } else {
        witness.closed_end_charts()
    };
    build_selected(
        triangles,
        layers,
        step_s,
        length_tolerance_m,
        witness.closed_start_charts(),
        end,
    )
}
pub(crate) fn build_closed_sample<S: TransientGasScalar>(
    triangles: &[LayeredGapTriangle<S>],
    layers: usize,
    step_s: S,
    length_tolerance_m: f64,
    sample: &crate::native_closed_release::NativeClosedReleaseSample,
) -> CaeResult<LayeredGasMesh<S>> {
    build_selected(
        triangles,
        layers,
        step_s,
        length_tolerance_m,
        sample.closed_start_charts(),
        sample.closed_end_charts(),
    )
}
pub(crate) fn build_closed_trial_sample<S: TransientGasScalar>(
    triangles: &[LayeredGapTriangle<S>],
    layers: usize,
    step_s: S,
    length_tolerance_m: f64,
    sample: &crate::native_closed_release::NativeClosedTrialSample,
) -> CaeResult<LayeredGasMesh<S>> {
    build_selected(
        triangles,
        layers,
        step_s,
        length_tolerance_m,
        sample.closed_start_charts(),
        sample.closed_end_charts(),
    )
}
pub(crate) fn build_closed_endpoint<S: TransientGasScalar>(
    triangles: &[LayeredGapTriangle<S>],
    layers: usize,
    step_s: S,
    length_tolerance_m: f64,
    start: &[usize],
    end: &[usize],
    fraction: f64,
) -> CaeResult<LayeredGasMesh<S>> {
    let closed: Vec<_> = if fraction == 0. {
        start.to_vec()
    } else if fraction == 1. {
        end.to_vec()
    } else {
        start
            .iter()
            .copied()
            .filter(|chart| end.contains(chart))
            .collect()
    };
    build_selected(
        triangles,
        layers,
        step_s,
        length_tolerance_m,
        &closed,
        &closed,
    )
}
fn build_selected<S: TransientGasScalar>(
    triangles: &[LayeredGapTriangle<S>],
    layers: usize,
    step_s: S,
    length_tolerance_m: f64,
    closed_start: &[usize],
    closed_end: &[usize],
) -> CaeResult<LayeredGasMesh<S>> {
    if closed_start
        .iter()
        .chain(closed_end)
        .any(|&chart| chart >= triangles.len())
    {
        return Err(fail("closed chart certificate metadata shape"));
    }
    if triangles.is_empty()
        || layers == 0
        || !step_s.finite_first_order()
        || step_s.value() <= 0.
        || !length_tolerance_m.is_finite()
        || length_tolerance_m < 0.
    {
        return Err(fail("invalid layered gas mesh settings"));
    }
    let count = triangles
        .len()
        .checked_mul(layers)
        .and_then(|n| n.checked_mul(3))
        .ok_or_else(|| fail("layered gas mesh size overflow"))?;
    let mut nodes: Vec<LayeredGasNode<S>> = vec![];
    let mut node_ids: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    let mut tets = Vec::with_capacity(count);
    let mut covers = BTreeMap::new();
    let mut sides: BTreeMap<[usize; 3], (usize, usize)> = BTreeMap::new();
    for (chart, triangle) in triangles.iter().enumerate() {
        let mut ids = triangle.paired_vertex_ids;
        ids.sort_unstable();
        if ids.windows(2).any(|w| w[0] == w[1])
            || triangle
                .start_m
                .iter()
                .chain(&triangle.end_m)
                .flatten()
                .any(|x| !x.finite_first_order())
        {
            return Err(fail("invalid layered gas triangle"));
        }
        let mut local = vec![[0usize; 3]; layers + 1];
        for corner in 0..3 {
            let fixed = (0..3).all(|axis| {
                let a = triangle.start_m[corner + 3][axis] - triangle.start_m[corner][axis];
                let b = triangle.end_m[corner + 3][axis] - triangle.end_m[corner][axis];
                a.value() == 0.
                    && a.maximum_direction() == 0.
                    && b.value() == 0.
                    && b.maximum_direction() == 0.
            });
            for layer in 0..=layers {
                let fraction = layer as f64 / layers as f64;
                let start_m = std::array::from_fn(|axis| {
                    triangle.start_m[corner][axis]
                        + (triangle.start_m[corner + 3][axis] - triangle.start_m[corner][axis])
                            * fraction
                });
                let end_m = std::array::from_fn(|axis| {
                    triangle.end_m[corner][axis]
                        + (triangle.end_m[corner + 3][axis] - triangle.end_m[corner][axis])
                            * fraction
                });
                let key = (
                    triangle.paired_vertex_ids[corner],
                    if fixed { 0 } else { layer },
                );
                let id = if let Some(&id) = node_ids.get(&key) {
                    if !equal(nodes[id].start_m, start_m, length_tolerance_m)
                        || !equal(nodes[id].end_m, end_m, length_tolerance_m)
                    {
                        return Err(fail(
                            "shared layered gas vertex geometry or direction differs",
                        ));
                    }
                    id
                } else {
                    let id = nodes.len();
                    nodes.push(LayeredGasNode {
                        paired_vertex: key.0,
                        layer: key.1,
                        start_m,
                        end_m,
                    });
                    node_ids.insert(key, id);
                    id
                };
                local[layer][corner] = id;
            }
        }
        for (level, kind) in [
            (0, LayeredGasBoundary::LowerCover { chart }),
            (layers, LayeredGasBoundary::UpperCover { chart }),
        ] {
            let key = sorted(local[level]);
            if covers.contains_key(&key) && sorted(local[0]) == sorted(local[layers]) {
                continue;
            }
            if covers.insert(key, kind).is_some() {
                return Err(fail("duplicate layered gas native cover"));
            }
        }
        let mut order = [0, 1, 2];
        order.sort_by_key(|&i| triangle.paired_vertex_ids[i]);
        for layer in 0..layers {
            let b = order.map(|i| local[layer][i]);
            let t = order.map(|i| local[layer + 1][i]);
            let mut oriented = determinant(
                sub(triangle.end_m[order[1]], triangle.end_m[order[0]]),
                sub(triangle.end_m[order[2]], triangle.end_m[order[0]]),
                std::array::from_fn(|axis| {
                    (0..3)
                        .map(|i| triangle.end_m[i + 3][axis] - triangle.end_m[i][axis])
                        .sum::<S>()
                        / 3.
                }),
            );
            if oriented.value() == 0. {
                oriented = determinant(
                    sub(triangle.start_m[order[1]], triangle.start_m[order[0]]),
                    sub(triangle.start_m[order[2]], triangle.start_m[order[0]]),
                    std::array::from_fn(|axis| {
                        (0..3)
                            .map(|i| triangle.start_m[i + 3][axis] - triangle.start_m[i][axis])
                            .sum::<S>()
                            / 3.
                    }),
                );
            }
            let mut templates = [
                [b[0], b[1], b[2], t[2]],
                [b[0], b[1], t[2], t[1]],
                [b[0], t[0], t[1], t[2]],
            ];
            if oriented.value() < 0. {
                for tet in &mut templates {
                    tet.swap(2, 3);
                }
            }
            for (local_tetra, node_set) in templates.into_iter().enumerate() {
                let mut unique = node_set;
                unique.sort_unstable();
                let duplicate = unique.windows(2).any(|w| w[0] == w[1]);
                let start = node_set.map(|i| nodes[i].start_m);
                let end = node_set.map(|i| nodes[i].end_m);
                let old = if duplicate {
                    S::zero()
                } else if closed_start.contains(&chart) {
                    certified_zero_volume(start)?
                } else {
                    volume(start)
                };
                let new = if duplicate {
                    S::zero()
                } else if closed_end.contains(&chart) {
                    certified_zero_volume(end)?
                } else {
                    volume(end)
                };
                if !old.finite_first_order()
                    || !new.finite_first_order()
                    || old.value() < 0.
                    || old.value() == 0. && old.maximum_direction() > 0.
                    || new.value() < 0.
                    || new.value() == 0. && new.maximum_direction() > 0.
                {
                    return Err(fail(
                        "layered gas cell is inverted or changes a closed stratum",
                    ));
                }
                if old.value() > 0. && new.value() == 0. {
                    return Err(fail(
                        "layered gas closure requires a drained localized event",
                    ));
                }
                if !duplicate {
                    certify_path(start, end, closed_start.contains(&chart))?;
                }
                tets.push(LayeredGasTetrahedron {
                    chart,
                    layer,
                    local_tetra,
                    nodes: node_set,
                    previous_volume_m3: old,
                    volume_m3: new,
                });
            }
            for edge in 0..3 {
                let next = (edge + 1) % 3;
                let (i, j) = if triangle.paired_vertex_ids[edge] < triangle.paired_vertex_ids[next]
                {
                    (edge, next)
                } else {
                    (next, edge)
                };
                let b0 = local[layer][i];
                let b1 = local[layer][j];
                let t0 = local[layer + 1][i];
                let t1 = local[layer + 1][j];
                for face in [[b0, b1, t1], [b0, t1, t0]] {
                    let key = sorted(face);
                    sides.entry(key).or_insert((chart, edge));
                }
            }
        }
    }
    let mut faces_by_nodes: BTreeMap<[usize; 3], Vec<(usize, [usize; 3])>> = BTreeMap::new();
    for (cell, tet) in tets.iter().enumerate() {
        if tet.volume_m3.value() == 0. {
            continue;
        }
        for face in [[1, 2, 3], [0, 3, 2], [0, 1, 3], [0, 2, 1]] {
            let ids = face.map(|i| tet.nodes[i]);
            faces_by_nodes
                .entry(sorted(ids))
                .or_default()
                .push((cell, ids));
        }
    }
    let mut faces = vec![];
    let gauss = [(1. - 1. / 3f64.sqrt()) / 2., (1. + 1. / 3f64.sqrt()) / 2.];
    let bary = [
        [2. / 3., 1. / 6., 1. / 6.],
        [1. / 6., 2. / 3., 1. / 6.],
        [1. / 6., 1. / 6., 2. / 3.],
    ];
    for (key, owners) in faces_by_nodes {
        if owners.len() > 2 {
            return Err(fail("layered gas face has more than two owners"));
        }
        let (left, ids) = owners[0];
        if owners.len() == 2 {
            let parity = |ids: [usize; 3]| {
                (0..3)
                    .flat_map(|i| (i + 1..3).map(move |j| (i, j)))
                    .filter(|&(i, j)| ids[i] > ids[j])
                    .count()
                    % 2
            };
            if parity(ids) == parity(owners[1].1) {
                return Err(fail("layered gas interior face orientations disagree"));
            }
        }
        let boundary = if owners.len() == 2 {
            LayeredGasBoundary::Interior { right: owners[1].0 }
        } else if let Some(kind) = covers.get(&key) {
            kind.clone()
        } else if let Some(&(chart, edge)) = sides.get(&key) {
            LayeredGasBoundary::Side { chart, edge }
        } else {
            return Err(fail(
                "layered gas face has no physical boundary classification",
            ));
        };
        for time in gauss {
            let positions = ids.map(|i| {
                std::array::from_fn(|axis| {
                    nodes[i].start_m[axis] + (nodes[i].end_m[axis] - nodes[i].start_m[axis]) * time
                })
            });
            let vector = cross(
                sub(positions[1], positions[0]),
                sub(positions[2], positions[0]),
            )
            .map(|x| x * 0.5);
            let area = dot(vector, vector).sqrt();
            if !area.finite_first_order() || area.value() <= 0. {
                return Err(fail(
                    "layered gas face degenerates inside its time interval",
                ));
            }
            let normal = vector.map(|x| x / area);
            for weights in bary {
                let position_m = std::array::from_fn(|axis| {
                    (0..3)
                        .map(|i| positions[i][axis] * weights[i])
                        .fold(S::zero(), |a, b| a + b)
                });
                let velocity_m_s = std::array::from_fn(|axis| {
                    (0..3)
                        .map(|i| {
                            (nodes[ids[i]].end_m[axis] - nodes[ids[i]].start_m[axis]) * weights[i]
                                / step_s
                        })
                        .fold(S::zero(), |a, b| a + b)
                });
                faces.push(LayeredGasFacePoint {
                    left,
                    boundary: boundary.clone(),
                    nodes: ids,
                    node_weights: weights,
                    time_fraction: time,
                    position_m,
                    area_m2: area / 6.,
                    normal,
                    velocity_m_s,
                });
            }
        }
    }
    let mut metric = vec![[S::zero(); 3]; tets.len()];
    let mut swept = vec![S::zero(); tets.len()];
    let mut swept_direction_scale = vec![0f64; tets.len()];
    let mut area_scale = vec![0f64; tets.len()];
    for face in &faces {
        let dv = step_s * face.area_m2 * dot(face.normal, face.velocity_m_s);
        for axis in 0..3 {
            metric[face.left][axis] += face.area_m2 * face.normal[axis];
        }
        swept[face.left] += dv;
        swept_direction_scale[face.left] += dv.maximum_direction();
        area_scale[face.left] += face.area_m2.value();
        if let LayeredGasBoundary::Interior { right } = face.boundary {
            for axis in 0..3 {
                metric[right][axis] -= face.area_m2 * face.normal[axis];
            }
            swept[right] -= dv;
            swept_direction_scale[right] += dv.maximum_direction();
            area_scale[right] += face.area_m2.value();
        }
    }
    let mut geometric = 0f64;
    let mut metric_error = 0f64;
    for (i, tet) in tets.iter().enumerate() {
        let defect = tet.volume_m3 - tet.previous_volume_m3 - swept[i];
        geometric = geometric.max(defect.value().abs());
        let volume_scale = tet
            .volume_m3
            .value()
            .max(tet.previous_volume_m3.value())
            .max(swept[i].value().abs());
        let tol = 4096. * f64::EPSILON * volume_scale;
        if !defect.finite_first_order()
            || defect.value().abs() > tol
            || defect.maximum_direction()
                > 4096.
                    * f64::EPSILON
                    * (tet.volume_m3.maximum_direction()
                        + tet.previous_volume_m3.maximum_direction()
                        + swept_direction_scale[i])
        {
            return Err(fail(
                "layered gas space-time faces violate volume conservation",
            ));
        }
        for m in metric[i] {
            metric_error = metric_error.max(m.value().abs());
            if !m.finite_first_order() || m.value().abs() > 4096. * f64::EPSILON * area_scale[i] {
                return Err(fail("layered gas space-time faces violate metric closure"));
            }
        }
    }
    Ok(LayeredGasMesh {
        nodes,
        tetrahedra: tets,
        faces,
        maximum_geometric_defect_m3: geometric,
        maximum_metric_defect_m2: metric_error,
    })
}
