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

use super::{NativeCornerConstruction, NativeCornerKind, NativePointOwner};
use crate::gas_transient_kinetic::TransientGasScalar;
use crate::native_cut_cell_partition::CutBoundary;
use crate::native_geometry_construction_error::{ConstructionJet, TrackedScalar};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

type Point<S> = [TrackedScalar<S>; 3];
type Plane<S> = (Point<S>, Point<S>, TrackedScalar<S>);

pub(super) struct CornerErrorProof {
    point: [ConstructionJet; 3],
    active_residuals: Vec<ConstructionJet>,
}
impl CornerErrorProof {
    pub(super) fn certifies_active_constraints(&self) -> bool {
        self.active_residuals
            .iter()
            .all(ConstructionJet::certifies_zero)
    }
    pub(super) fn point(&self) -> &[ConstructionJet; 3] {
        &self.point
    }
}

fn dot<S: TransientGasScalar>(a: Point<S>, b: Point<S>) -> TrackedScalar<S> {
    (0..3)
        .map(|k| a[k] * b[k])
        .fold(TrackedScalar::zero(), |a, b| a + b)
}
fn sub<S: TransientGasScalar>(a: Point<S>, b: Point<S>) -> Point<S> {
    std::array::from_fn(|k| a[k] - b[k])
}
fn cross<S: TransientGasScalar>(a: Point<S>, b: Point<S>) -> Point<S> {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(super) fn replay<S: TransientGasScalar>(
    construction: &NativeCornerConstruction<S>,
    parent: impl Fn(NativePointOwner) -> CaeResult<Point<S>>,
    plane: impl Fn(&CutBoundary) -> CaeResult<Plane<S>>,
) -> CaeResult<CornerErrorProof> {
    let active: Vec<_> = construction
        .active
        .iter()
        .map(|b| plane(b).map(|p| (p, b)))
        .collect::<CaeResult<_>>()?;
    let mut point = match &construction.kind {
        NativeCornerKind::Parent(owner) => parent(*owner)?,
        NativeCornerKind::Edge {
            left,
            right,
            plane: boundary,
        } => {
            let left = parent(*left)?;
            let edge = sub(parent(*right)?, left);
            let (normal, origin, offset) = plane(boundary)?;
            let denominator = dot(normal, edge);
            let parameter = (-dot(normal, sub(left, origin)) - offset) / denominator;
            std::array::from_fn(|k| left[k] + parameter * edge[k])
        }
        NativeCornerKind::PlaneIntersection {
            selected,
            anchor,
            delta: recorded_delta,
        } => {
            let selected = selected.iter().map(&plane).collect::<CaeResult<Vec<_>>>()?;
            let normal: [Point<S>; 3] =
                std::array::from_fn(|i| selected[i].0.map(TrackedScalar::freeze_value));
            let determinant = dot(normal[0], cross(normal[1], normal[2]));
            let anchor = plane(anchor)?.1;
            let anchor_value = anchor.map(TrackedScalar::freeze_value);
            let rhs: [TrackedScalar<S>; 3] = std::array::from_fn(|i| {
                (-dot(selected[i].0, sub(anchor_value, selected[i].1)) - selected[i].2)
                    .freeze_value()
            });
            let delta: Point<S> = std::array::from_fn(|k| {
                (cross(normal[1], normal[2])[k] * rhs[0]
                    + cross(normal[2], normal[0])[k] * rhs[1]
                    + cross(normal[0], normal[1])[k] * rhs[2])
                    / determinant
            });
            if (0..3).any(|k| delta[k].value().to_bits() != recorded_delta[k].to_bits()) {
                return Err(CaeError::contract(
                    "corner local rank-three replay correction differs",
                ));
            }
            let relative = std::array::from_fn(|k| anchor[k] + delta[k]);
            let residual: [TrackedScalar<S>; 3] = std::array::from_fn(|i| {
                let r = -dot(selected[i].0, sub(relative, selected[i].1)) - selected[i].2;
                r - r.freeze_value()
            });
            let moving: [Point<S>; 3] = std::array::from_fn(|i| selected[i].0);
            let moving_det = dot(moving[0], cross(moving[1], moving[2]));
            std::array::from_fn(|k| {
                relative[k]
                    + (cross(moving[1], moving[2])[k] * residual[0]
                        + cross(moving[2], moving[0])[k] * residual[1]
                        + cross(moving[0], moving[1])[k] * residual[2])
                        / moving_det
            })
        }
    };
    for axis in 0..3 {
        let mut supported = active
            .iter()
            .filter(|((normal, _, _), _)| {
                normal[axis].value() != 0.
                    && (0..3).filter(|&k| k != axis).all(|k| {
                        normal[k].value() == 0.
                            && normal[k]
                                .geometry_first_order_directions()
                                .is_some_and(|d| d.iter().all(|&v| v == 0.))
                    })
            })
            .collect::<Vec<_>>();
        supported.sort_by_key(|(_, boundary)| match boundary {
            CutBoundary::Native { .. } => 0,
            CutBoundary::Cell { .. } => 1,
            CutBoundary::Subdomain { .. } => 2,
        });
        if let Some(((normal, origin, offset), _)) = supported.first() {
            point[axis] = origin[axis] - *offset / normal[axis];
        }
    }
    for k in 0..3 {
        let actual = point[k].actual();
        let expected = construction.actual[k];
        if actual.value().to_bits() != expected.value().to_bits()
            || actual
                .geometry_first_order_directions()
                .map(|d| d.iter().map(|v| v.to_bits()).collect::<Vec<_>>())
                != expected
                    .geometry_first_order_directions()
                    .map(|d| d.iter().map(|v| v.to_bits()).collect::<Vec<_>>())
        {
            return Err(CaeError::contract(
                "corner local construction replay output differs",
            ));
        }
    }
    let point = point.map(TrackedScalar::construction_proof);
    let [x, y, z] = point;
    let point = [x?, y?, z?];
    let mut active_residuals = Vec::with_capacity(active.len());
    let actual_point: [CaeResult<TrackedScalar<S>>; 3] =
        std::array::from_fn(|k| TrackedScalar::from_bound(construction.actual[k], &point[k]));
    let [x, y, z] = actual_point;
    let actual_point = [x?, y?, z?];
    for ((normal, origin, offset), _) in active {
        active_residuals
            .push((dot(normal, sub(actual_point, origin)) + offset).construction_proof()?);
    }
    Ok(CornerErrorProof {
        point,
        active_residuals,
    })
}

pub(super) fn native_parent<S: TransientGasScalar>(
    owner: &crate::native_gap_column_owner::NativeGapColumnOwner,
    trace: &[TrackedScalar<S>],
    body: usize,
    node: usize,
) -> CaeResult<Point<S>> {
    let binding = owner
        .nodes
        .get(body)
        .and_then(|nodes| nodes.get(node))
        .ok_or_else(|| CaeError::contract("corner local native parent is absent"))?;
    let mut point = [TrackedScalar::zero(); 3];
    for k in 0..3 {
        point[k] = TrackedScalar::from_f64(binding.reference_m[k])
            + *trace
                .get(binding.force_flux[k])
                .ok_or_else(|| CaeError::contract("corner local native trace index is absent"))?;
    }
    Ok(point)
}

pub(super) fn native_plane<S: TransientGasScalar>(
    owner: &crate::native_gap_column_owner::NativeGapColumnOwner,
    cell: usize,
    boundary: &CutBoundary,
    trace: &[TrackedScalar<S>],
    design: &[TrackedScalar<S>],
    tet: impl Fn(
        usize,
    )
        -> CaeResult<crate::native_cut_cell_refinement::CutTetraSubdomain<TrackedScalar<S>>>,
) -> CaeResult<Plane<S>> {
    let (normal, origin, offset) = match boundary {
        CutBoundary::Cell { axis, side } => {
            let bounds = owner.grid.cell_bounds(cell)?;
            (
                std::array::from_fn(|k| TrackedScalar::from_f64(if k == *axis { 1. } else { 0. })),
                bounds[*side].map(TrackedScalar::from_f64),
                TrackedScalar::zero(),
            )
        }
        CutBoundary::Subdomain { region, face } => {
            let tet = tet(*region)?;
            if tet.region != *region {
                return Err(CaeError::contract("corner local tet owner differs"));
            }
            let p = crate::native_cut_cell_refinement::canonical_face(&tet, *face)?;
            (
                cross(sub(p[1], p[0]), sub(p[2], p[0])),
                p[0],
                TrackedScalar::zero(),
            )
        }
        CutBoundary::Native {
            body,
            element,
            face,
            ..
        } => {
            if let Some(face) = face {
                let mut ids = *face;
                ids.sort_unstable();
                let p = ids.map(|node| native_parent(owner, trace, *body, node));
                let [a, b, c] = p;
                let p = [a?, b?, c?];
                (
                    cross(sub(p[1], p[0]), sub(p[2], p[0])),
                    p[0],
                    TrackedScalar::zero(),
                )
            } else {
                let ids = *owner
                    .elements
                    .get(*body)
                    .and_then(|elements| elements.get(*element))
                    .ok_or_else(|| CaeError::contract("corner local phase element is absent"))?;
                let p = ids.map(|node| native_parent(owner, trace, *body, node));
                let [a, b, c, d] = p;
                let p = [a?, b?, c?, d?];
                let row_offset = if *body == 0 { 0 } else { owner.nodes[0].len() };
                let rho = ids.map(|node| {
                    let (columns, weights) = owner.phase.row(row_offset + node);
                    columns
                        .iter()
                        .zip(weights)
                        .map(|(&j, &weight)| design[j] * weight)
                        .fold(TrackedScalar::zero(), |a, b| a + b)
                });
                let a = sub(p[1], p[0]);
                let b = sub(p[2], p[0]);
                let c = sub(p[3], p[0]);
                let det = dot(a, cross(b, c));
                let normal = std::array::from_fn(|k| {
                    ((rho[1] - rho[0]) * cross(b, c)[k]
                        + (rho[2] - rho[0]) * cross(c, a)[k]
                        + (rho[3] - rho[0]) * cross(a, b)[k])
                        / det
                });
                (normal, p[0], rho[0] - owner.iso)
            }
        }
    };
    let axis = (0..3)
        .max_by(|&a, &b| normal[a].value().abs().total_cmp(&normal[b].value().abs()))
        .unwrap();
    let length = normal[axis];
    if !length.value().is_finite() || length.value() == 0. {
        return Err(CaeError::contract(
            "corner local authentic plane is degenerate",
        ));
    }
    Ok((normal.map(|v| v / length), origin, offset / length))
}

pub(super) fn issue<S: TransientGasScalar>(
    construction: &NativeCornerConstruction<S>,
    owner: &crate::native_gap_column_owner::NativeGapColumnOwner,
    kinematics: &crate::surface_gap::SurfaceGapKinematics,
    layers: usize,
    trace: &[S],
    design: &[S],
    layout: crate::native_geometry_construction_error::JetLayout,
) -> CaeResult<CornerErrorProof> {
    use crate::native_geometry_construction_error::ConstructionScope;
    let _scope = ConstructionScope::new(layout)?;
    let trace: Vec<_> = trace
        .iter()
        .copied()
        .map(TrackedScalar::from_control)
        .collect();
    let design: Vec<_> = design
        .iter()
        .copied()
        .map(TrackedScalar::from_control)
        .collect();
    let charts = std::cell::RefCell::new(std::collections::BTreeMap::new());
    let layered = |id: usize| -> CaeResult<Point<S>> {
        let &(_, chart, corner, layer) = construction
            .layered_sources
            .iter()
            .find(|source| source.0 == id)
            .ok_or_else(|| CaeError::contract("corner local layered source is absent"))?;
        if layers == 0 || layer > layers || corner >= 3 {
            return Err(CaeError::contract("corner local layered source is invalid"));
        }
        if !charts.borrow().contains_key(&chart) {
            let points =
                kinematics
                    .mapped_chart_from_trace(chart, &trace, &design, |points, _| Ok(points))?;
            charts.borrow_mut().insert(chart, points);
        }
        let points = *charts
            .borrow()
            .get(&chart)
            .ok_or_else(|| CaeError::contract("corner local chart source is absent"))?;
        let fraction = layer as f64 / layers as f64;
        Ok(std::array::from_fn(|axis| {
            points[corner][axis] + (points[corner + 3][axis] - points[corner][axis]) * fraction
        }))
    };
    let parent = |parent: NativePointOwner| match parent {
        NativePointOwner::Native(body, node) => native_parent(owner, &trace, body, node),
        NativePointOwner::Layered(id) => layered(id),
    };
    let plane = |boundary: &CutBoundary| {
        native_plane(
            owner,
            construction.cell,
            boundary,
            &trace,
            &design,
            |region| {
                let &(_, ids) = construction
                    .tet_vertices
                    .iter()
                    .find(|entry| entry.0 == region)
                    .ok_or_else(|| CaeError::contract("corner local numerical tet is absent"))?;
                let points = ids.map(&layered);
                let [a, b, c, d] = points;
                Ok(crate::native_cut_cell_refinement::CutTetraSubdomain {
                    region,
                    vertex_ids: Some(ids),
                    vertices_m: [a?, b?, c?, d?],
                })
            },
        )
    };
    replay(construction, parent, plane)
}
