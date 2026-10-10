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
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub struct UnionNode<S: Scalar> {
    pub feature: Vec<usize>,
    pub point: [S; 2],
}

#[derive(Clone, Debug)]
pub struct UnionEdge {
    pub endpoints: [usize; 2],
    pub physical_owner: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct OwnedPartitionEdge {
    pub endpoints: [usize; 2],
    pub support: Vec<usize>,
    pub physical_owner: Option<Vec<usize>>,
}

pub fn cancel_owned_partition_edges(edges: &[OwnedPartitionEdge]) -> Result<Vec<UnionEdge>, String> {
    let mut retained = BTreeMap::<(Vec<usize>, usize, usize), &OwnedPartitionEdge>::new();
    let mut completed = BTreeSet::new();
    for edge in edges {
        let [a, b] = edge.endpoints;
        if a == b || edge.support.is_empty() {
            return Err("partition edge has no distinct authenticated endpoints/support".into());
        }
        let pair = (edge.support.clone(), a.min(b), a.max(b));
        if completed.contains(&pair) {
            return Err("partition union edge has more than two incident pieces".into());
        }
        if let Some(previous) = retained.remove(&pair) {
            if previous.endpoints != [b, a] || previous.physical_owner.is_some() || edge.physical_owner.is_some() {
                return Err("partition edge cancellation conflicts with orientation or physical ownership".into());
            }
            completed.insert(pair);
        } else {
            retained.insert(pair, edge);
        }
    }
    retained.into_values().map(|edge| {
        Ok(UnionEdge {
            endpoints: edge.endpoints,
            physical_owner: edge.physical_owner.clone().filter(|owner| !owner.is_empty()).ok_or("uncanceled artificial partition edge")?,
        })
    }).collect()
}

#[derive(Clone, Debug)]
pub struct UnionSample<S: Scalar> {
    pub point: [S; 2],
    pub weight: S,
}

pub fn positive_union_quadrature_in_frame<S: Scalar>(
    nodes: &[UnionNode<S>],
    edges: &[UnionEdge],
    refinement: usize,
    frame: [f64; 2],
    same_motion: impl Fn(S, S) -> bool,
) -> Result<Vec<UnionSample<S>>, String> {
    let [c, s] = frame;
    let determinant = c * c + s * s;
    if !determinant.is_finite() || determinant <= 0. {
        return Err("invalid fixed union sweep frame".into());
    }
    let rotated: Vec<_> = nodes.iter().map(|node| UnionNode {
        feature: node.feature.clone(),
        point: [node.point[0] * c + node.point[1] * s, node.point[1] * c - node.point[0] * s],
    }).collect();
    positive_union_quadrature(&rotated, edges, refinement, same_motion).map(|samples| {
        samples.into_iter().map(|sample| UnionSample {
            point: [(sample.point[0] * c - sample.point[1] * s) / determinant,
                    (sample.point[0] * s + sample.point[1] * c) / determinant],
            weight: sample.weight / determinant,
        }).collect()
    })
}

fn subtract<S: Scalar>(a: [S; 2], b: [S; 2]) -> [S; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

pub(crate) fn triangle<S: Scalar>(
    vertices: [[S; 2]; 3],
    refinement: usize,
    same_motion: &impl Fn(S, S) -> bool,
    samples: &mut Vec<UnionSample<S>>,
) -> Result<(), String> {
    let a = subtract(vertices[1], vertices[0]);
    let b = subtract(vertices[2], vertices[0]);
    let area = (a[0] * b[1] - a[1] * b[0]) * 0.5;
    if !area.value().is_finite() || area.value() < 0. {
        return Err("union triangle has nonpositive orientation".into());
    }
    if area.value() == 0. {
        if !same_motion(area, S::zero()) {
            return Err("zero union triangle has a nonzero area direction".into());
        }
        return Ok(());
    }
    if refinement > 0 {
        let midpoint = |a: [S; 2], b: [S; 2]| [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        let ab = midpoint(vertices[0], vertices[1]);
        let bc = midpoint(vertices[1], vertices[2]);
        let ca = midpoint(vertices[2], vertices[0]);
        for child in [
            [vertices[0], ab, ca],
            [ab, vertices[1], bc],
            [ca, bc, vertices[2]],
            [ab, bc, ca],
        ] {
            triangle(child, refinement - 1, same_motion, samples)?;
        }
        return Ok(());
    }
    for weights in [[2. / 3., 1. / 6., 1. / 6.], [1. / 6., 2. / 3., 1. / 6.], [1. / 6., 1. / 6., 2. / 3.]] {
        samples.push(UnionSample {
            point: std::array::from_fn(|k| {
                (0..3).map(|j| vertices[j][k] * weights[j]).fold(S::zero(), |a, b| a + b)
            }),
            weight: area / 3.,
        });
    }
    Ok(())
}

pub fn positive_union_quadrature<S: Scalar>(
    nodes: &[UnionNode<S>],
    edges: &[UnionEdge],
    refinement: usize,
    same_motion: impl Fn(S, S) -> bool,
) -> Result<Vec<UnionSample<S>>, String> {
    if nodes.len() < 3 || edges.is_empty() || refinement > 8 {
        return Err("invalid union quadrature boundary or refinement".into());
    }
    let mut features = BTreeSet::new();
    let mut incidence = BTreeMap::<usize, [usize; 2]>::new();
    for node in nodes {
        if node.feature.is_empty() || !features.insert(node.feature.clone()) || node.point.iter().any(|x| !x.value().is_finite()) {
            return Err("union boundary node identity is absent or ambiguous".into());
        }
    }
    let mut edge_identities = BTreeSet::new();
    for edge in edges {
        let [a, b] = edge.endpoints;
        if a >= nodes.len() || b >= nodes.len() || a == b || edge.physical_owner.is_empty() {
            return Err("union boundary edge ownership is invalid".into());
        }
        if !edge_identities.insert((a, b)) || edge_identities.contains(&(b, a)) {
            return Err("uncanceled internal union boundary edge".into());
        }
        incidence.entry(a).or_default()[1] += 1;
        incidence.entry(b).or_default()[0] += 1;
    }
    if incidence.len() != nodes.len() || incidence.values().any(|&degree| degree != [1, 1]) {
        return Err("union physical boundary does not form disjoint oriented loops".into());
    }
    let mut columns: Vec<usize> = (0..nodes.len()).collect();
    columns.sort_by(|&a, &b| nodes[a].point[0].value().total_cmp(&nodes[b].point[0].value()).then_with(|| nodes[a].feature.cmp(&nodes[b].feature)));
    let mut unique = Vec::<usize>::new();
    for node in columns {
        if let Some(&previous) = unique.last() {
            if nodes[node].point[0].value() == nodes[previous].point[0].value() {
                if !same_motion(nodes[node].point[0], nodes[previous].point[0]) {
                    return Err("coincident union sweep columns have incompatible motions".into());
                }
                continue;
            }
        }
        unique.push(node);
    }
    let mut samples = Vec::new();
    for slab in unique.windows(2) {
        let left = nodes[slab[0]].point[0];
        let right = nodes[slab[1]].point[0];
        let middle = (left.value() + right.value()) * 0.5;
        let mut crossings = Vec::new();
        for (index, edge) in edges.iter().enumerate() {
            let [a, b] = edge.endpoints.map(|i| nodes[i].point);
            if a[0].value().min(b[0].value()) < middle && middle < a[0].value().max(b[0].value()) {
                let fraction = (middle - a[0].value()) / (b[0].value() - a[0].value());
                crossings.push((a[1].value() + (b[1].value() - a[1].value()) * fraction, index, if b[0].value() > a[0].value() {1_i32} else {-1_i32}));
            }
        }
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        if crossings.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err("coincident physical boundary crossings require an event owner".into());
        }
        let edge_at = |edge: usize, x: S| {
            let [a, b] = edges[edge].endpoints.map(|i| nodes[i].point);
            if x.value()==a[0].value() && same_motion(x,a[0]) {return a;}
            if x.value()==b[0].value() && same_motion(x,b[0]) {return b;}
            let fraction = (x - a[0]) / (b[0] - a[0]);
            [x, a[1] + (b[1] - a[1]) * fraction]
        };
        let mut winding = 0;
        let mut lower = None;
        for (_, edge, change) in crossings {
            let previous = winding;
            winding += change;
            if winding < 0 || winding > 1 {
                return Err("union boundary winding is not a disjoint positive gas footprint".into());
            }
            if previous == 0 && winding == 1 {
                lower = Some(edge);
            }
            if previous == 1 && winding == 0 {
                let lower = lower.take().ok_or("union lower boundary is absent")?;
                let a = edge_at(lower, left);
                let b = edge_at(lower, right);
                let c = edge_at(edge, right);
                let d = edge_at(edge, left);
                triangle([a, b, c], refinement, &same_motion, &mut samples)?;
                triangle([a, c, d], refinement, &same_motion, &mut samples)?;
            }
        }
        if winding != 0 || lower.is_some() {
            return Err("union boundary winding does not close".into());
        }
    }
    Ok(samples)
}
