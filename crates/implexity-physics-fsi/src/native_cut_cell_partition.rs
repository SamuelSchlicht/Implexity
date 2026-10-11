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
use crate::{
    moving_contact::native_surface_binding::NativeNodalBinding,
    native_gap_column_owner::{NativeGapColumnGrid, NativeGapColumnOwner},
};
use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use std::collections::{BTreeMap, BTreeSet};
#[path = "native_cut_local_geometry.rs"]
mod local_geometry;
pub use local_geometry::CutLocalGeometry;
pub(crate) use local_geometry::{NativePlaneFace,NativeLevelsetPlane,SubdomainPlaneFace,SubdomainNativeSupport,PointConstruction,plane_origin_in_frame};
pub(crate) use local_geometry::{
    seed_vertex_plane, authentic_edge_source, bind_polygon_frame, framed_point_same, local_polygon_area, local_positive_polygon, polygon_plane_origin_in_frame,
    polygon_vertices_in_frame,
};
fn reference_local_geometry<S: Scalar>(face: &CutPolygon<S>) -> Option<CutLocalGeometry<S>> {
    face.local_geometry.as_ref().map(|g| {
        let mut g = g.clone();
        if let Some(&point) = g.vertices_m.first() {
            g.plane_origin_m = point;
        }
        g
    })
}
fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
fn sub<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    (0..3).map(|i| a[i] * b[i]).fold(S::zero(), |a, b| a + b)
}
fn cross<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn primal<S: Scalar>(p: [S; 3]) -> [f64; 3] {
    p.map(|x| x.value())
}
fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CutBoundary {
    Subdomain {
        region: usize,
        face: usize,
    },
    Cell {
        axis: usize,
        side: usize,
    },
    Native {
        body: usize,
        element: usize,
        face: Option<[usize; 3]>,
        physical: bool,
    },
}
#[derive(Clone, Debug)]
pub struct CutPolygon<S: Scalar> {
    pub support_only: bool,
    pub local_geometry: Option<CutLocalGeometry<S>>,
    pub vertices_m: Vec<[S; 3]>,
    pub outward_normal: [S; 3],
    pub plane_origin_m: [S; 3],
    pub plane_offset_m: S,
    pub boundary: CutBoundary,
}
#[derive(Clone, Debug)]
pub struct CutCellComponent<S: Scalar> {
    pub cell: usize,
    pub component: usize,
    pub volume_m3: S,
    pub centroid_m: [S; 3],
    pub local_centroid_m: Option<([f64; 3], [S; 3])>,
    pub central_second_integral_m5: [[S; 3]; 3],
    pub convex_parts: Vec<Vec<CutPolygon<S>>>,
}
#[derive(Clone, Debug)]
pub struct CutCellAperture<S: Scalar> {
    pub local_polygons: Option<Vec<CutLocalGeometry<S>>>,
    pub left: usize,
    pub right: Option<usize>,
    pub outside_cell: Option<usize>,
    pub axis: Option<usize>,
    pub normal: [S; 3],
    pub area_m2: S,
    pub polygons: Vec<Vec<[S; 3]>>,
    pub polygon_parts: Vec<(usize, Option<usize>)>,
}
#[derive(Clone, Debug)]
pub struct CutCellWall<S: Scalar> {
    pub local_geometry: Option<CutLocalGeometry<S>>,
    pub component: usize,
    pub convex_part: usize,
    pub body: usize,
    pub element: usize,
    pub area_m2: S,
    pub normal: [S; 3],
    pub vertices_m: Vec<[S; 3]>,
    pub native_nodes: [usize; 4],
    pub native_barycentric: Vec<[S; 4]>,
}
#[derive(Clone, Debug)]
pub struct CutWallQuadrature<S: Scalar> {
    pub wall: usize,
    pub component: usize,
    pub body: usize,
    pub native_nodes: [usize; 4],
    pub position_m: [S; 3],
    pub local_position_m: Option<([f64; 3], [S; 3])>,
    pub native_barycentric: [S; 4],
    pub area_m2: S,
    pub normal: [S; 3],
}
#[derive(Clone, Debug)]
pub struct CutApertureQuadrature<S: Scalar> {
    pub aperture: usize,
    pub left: usize,
    pub right: Option<usize>,
    pub position_m: [S; 3],
    pub local_position_m: Option<([f64; 3], [S; 3])>,
    pub area_m2: S,
    pub normal: [S; 3],
}
impl<S: Scalar> NativeCutCellPartition<S> {
    pub fn aperture_quadrature(&self) -> CaeResult<Vec<CutApertureQuadrature<S>>> {
        self.aperture_rule(&[[1. / 3.; 3]], None)
    }
    pub fn aperture_quadrature_degree2(&self) -> CaeResult<Vec<CutApertureQuadrature<S>>> {
        self.aperture_rule(
            &[
                [2. / 3., 1. / 6., 1. / 6.],
                [1. / 6., 2. / 3., 1. / 6.],
                [1. / 6., 1. / 6., 2. / 3.],
            ],
            None,
        )
    }
    pub(crate) fn aperture_moment_quadrature(&self) -> CaeResult<Vec<CutApertureQuadrature<S>>> {
        let (rule, weights) = triangle_moment_rule();
        self.aperture_rule(&rule, Some(&weights))
    }
    fn aperture_rule(
        &self,
        rule: &[[f64; 3]],
        weights: Option<&[f64]>,
    ) -> CaeResult<Vec<CutApertureQuadrature<S>>> {
        let mut out = Vec::new();
        for (aperture, a) in self.apertures.iter().enumerate() {
            if a.local_polygons.as_ref().is_some_and(|p| p.len() != a.polygons.len() || p.iter().zip(&a.polygons).any(|(local, world)| local.vertices().len() != world.len() || local.anchor().iter().any(|v| !v.is_finite()) || local.vertices().iter().flatten().any(|v| !v.value().is_finite()))) {
                return Err(fail("aperture local polygon layout or values differ"));
            }
            for (polygon_index, polygon) in a.polygons.iter().enumerate() {
                let local = a.local_polygons.as_ref().map(|p| &p[polygon_index]);
                let vertices = local.map_or(polygon.as_slice(), |p| p.vertices());
                for i in 1..vertices.len().saturating_sub(1) {
                    let p = [vertices[0], vertices[i], vertices[i + 1]];
                    let vector = area_vector(&p);
                    let weight = area(&p);
                    let orientation = if dot(vector, a.normal).value() < 0. {
                        -1.
                    } else {
                        1.
                    };
                    if weight.value() > 0. {
                        for (index, bary) in rule.iter().enumerate() {
                            let position = std::array::from_fn(|k| {
                                (0..3)
                                    .map(|j| p[j][k] * bary[j])
                                    .fold(S::zero(), |a, b| a + b)
                            });
                            out.push(CutApertureQuadrature {
                                aperture,
                                left: a.left,
                                right: a.right,
                                position_m: std::array::from_fn(|k| {
                                    position[k] + S::from_f64(local.map_or(0., |p| p.anchor()[k]))
                                }),
                                local_position_m: local.map(|p| (p.anchor(), position)),
                                area_m2: weights.map_or_else(
                                    || weight / rule.len() as f64,
                                    |w| weight * w[index],
                                ),
                                normal: crate::native_normalization::normalize(
                                    vector,
                                    weight,
                                    crate::native_normalization::NormalFormula::OrientedArea(
                                        orientation,
                                    ),
                                ),
                            });
                        }
                    }
                }
            }
        }
        Ok(out)
    }
    pub fn wall_quadrature(&self) -> CaeResult<Vec<CutWallQuadrature<S>>> {
        self.wall_rule(&[[1. / 3.; 3]], None)
    }
    pub fn wall_quadrature_degree2(&self) -> CaeResult<Vec<CutWallQuadrature<S>>> {
        self.wall_rule(
            &[
                [2. / 3., 1. / 6., 1. / 6.],
                [1. / 6., 2. / 3., 1. / 6.],
                [1. / 6., 1. / 6., 2. / 3.],
            ],
            None,
        )
    }
    pub(crate) fn wall_moment_quadrature(&self) -> CaeResult<Vec<CutWallQuadrature<S>>> {
        let (rule, weights) = triangle_moment_rule();
        self.wall_rule(&rule, Some(&weights))
    }
    fn wall_rule(&self, rule: &[[f64; 3]], weights: Option<&[f64]>) -> CaeResult<Vec<CutWallQuadrature<S>>> {
        let mut out = Vec::new();
        for (wall, w) in self.walls.iter().enumerate() {
            let local = w.local_geometry.as_ref();
            if w.native_barycentric.len() != w.vertices_m.len() || local.is_some_and(|p| p.vertices().len() != w.vertices_m.len() || p.anchor().iter().any(|v| !v.is_finite()) || p.vertices().iter().flatten().any(|v| !v.value().is_finite())) {
                return Err(fail("wall local polygon layout or values differ"));
            }
            let vertices = local.map_or(w.vertices_m.as_slice(), |p| p.vertices());
            for i in 1..vertices.len().saturating_sub(1) {
                let ids = [0, i, i + 1];
                let p = ids.map(|j| vertices[j]);
                let vector = area_vector(&p);
                let a = area(&p);
                let orientation = if dot(vector, w.normal).value() < 0. {
                    -1.
                } else {
                    1.
                };
                if a.value() > 0. {
                    for (index, bary) in rule.iter().enumerate() {
                        let position = std::array::from_fn(|k| {
                            (0..3).map(|j| p[j][k] * bary[j]).fold(S::zero(), |a,b| a+b)
                        });
                        out.push(CutWallQuadrature {
                            wall,
                            component: w.component,
                            body: w.body,
                            native_nodes: w.native_nodes,
                            position_m: std::array::from_fn(|k| position[k] + S::from_f64(local.map_or(0., |p| p.anchor()[k]))),
                            local_position_m: local.map(|p| (p.anchor(),position)),
                            native_barycentric: std::array::from_fn(|k| {
                                (0..3)
                                    .map(|j| w.native_barycentric[ids[j]][k] * bary[j])
                                    .fold(S::zero(), |a, b| a + b)
                            }),
                            area_m2: weights
                                .map_or_else(|| a / rule.len() as f64, |w| a * w[index]),
                            normal: vector.map(|v| v * orientation / a),
                        });
                    }
                }
            }
        }
        Ok(out)
    }
}
#[derive(Clone, Debug)]
pub struct NativeCutCellPartition<S: Scalar> {
    pub native_capacity_m3: BTreeMap<usize, S>,
    pub components: Vec<CutCellComponent<S>>,
    pub apertures: Vec<CutCellAperture<S>>,
    pub walls: Vec<CutCellWall<S>>,
    pub total_volume_m3: S,
    pub maximum_volume_defect_m3: f64,
    pub arithmetic_length_m: f64,
}
#[derive(Clone)]
pub struct NativeCutCellOwner {
    capacity: NativeGapColumnOwner,
}
#[derive(Clone)]
pub(crate) struct Plane<S: Scalar> {
    pub(crate) native_face: Option<NativePlaneFace<S>>,
    pub(crate) native_levelset: Option<NativeLevelsetPlane<S>>,
    pub(crate) subdomain_face: Option<SubdomainPlaneFace<S>>,
    pub(crate) local_origin: Option<([f64; 3], [S; 3])>,
    pub(crate) normal: [S; 3],
    pub(crate) origin: [S; 3],
    pub(crate) offset: S,
    pub(crate) boundary: CutBoundary,
}
fn area_vector<S: Scalar>(p: &[[S; 3]]) -> [S; 3] {
    let mut a = [S::zero(); 3];
    if p.len() < 3 {
        return a;
    }
    for i in 1..p.len() - 1 {
        let c = cross(sub(p[i], p[0]), sub(p[i + 1], p[0]));
        for k in 0..3 {
            a[k] += c[k] * 0.5;
        }
    }
    a
}
pub(crate) fn area<S: Scalar>(p: &[[S; 3]]) -> S {
    let a = area_vector(p);
    if dot(a, a).value() == 0. {
        S::zero()
    } else {
        dot(a, a).sqrt()
    }
}
pub(crate) fn positive_polygon<S: Scalar>(p: &[[S; 3]], eps: f64) -> bool {
    if p.len() < 3 {
        return false;
    }
    let p0 = primal(p[0]);
    let edge = p
        .iter()
        .map(|x| sub(primal(*x), p0))
        .max_by(|a, b| norm(*a).total_cmp(&norm(*b)))
        .unwrap();
    let length = norm(edge);
    if length <= eps {
        return false;
    }
    p.iter()
        .map(|x| norm(cross(edge, sub(primal(*x), p0))))
        .fold(0., f64::max)
        > eps * length
}
fn clean<S: Scalar>(mut p: Vec<[S; 3]>, eps: f64) -> Vec<[S; 3]> {
    let mut out = Vec::new();
    for v in p.drain(..) {
        if out
            .last()
            .is_none_or(|last| norm(sub(primal(v), primal(*last))) > eps)
        {
            out.push(v);
        }
    }
    if out.len() > 1 && norm(sub(primal(out[0]), primal(*out.last().unwrap()))) <= eps {
        out.pop();
    }
    out
}
fn unit<S: Scalar>(a: [S; 3]) -> CaeResult<[S; 3]> {
    let n = dot(a, a).sqrt();
    if !n.value().is_finite() || n.value() <= 0. {
        return Err(fail("invalid cut-cell plane"));
    }
    Ok(a.map(|x| x / n))
}
fn distance<S: Scalar>(plane: &Plane<S>, p: [S; 3], eps: f64) -> S {
    let d = dot(plane.normal, sub(p, plane.origin)) + plane.offset;
    if d.value().abs() <= eps {
        crate::native_producer_rounding::producer_snap(d)
    } else {
        d
    }
}
pub(crate) fn clip_polygon<S: Scalar>(
    p: &[[S; 3]],
    plane: &Plane<S>,
    eps: f64,
) -> CaeResult<Vec<[S; 3]>> {
    let mut out = Vec::new();
    if p.is_empty() {
        return Ok(out);
    }
    for i in 0..p.len() {
        let a = p[i];
        let b = p[(i + 1) % p.len()];
        let da = distance(plane, a, eps);
        let db = distance(plane, b, eps);
        if !da.value().is_finite() || !db.value().is_finite() {
            return Err(fail("nonfinite cut-cell clipping distance"));
        }
        if da.value() >= 0. {
            out.push(a);
        }
        if (da.value() >= 0.) != (db.value() >= 0.) {
            let t = da / (da - db);
            out.push(std::array::from_fn(|k| a[k] + (b[k] - a[k]) * t));
        }
    }
    Ok(clean(out, eps))
}
pub(crate) fn clip<S: TransientGasScalar>(
    faces: &[CutPolygon<S>],
    plane: &Plane<S>,
    eps: f64,
) -> CaeResult<Vec<CutPolygon<S>>> {
    if let Some(anchor) = faces
        .iter()
        .find_map(|f| f.local_geometry.as_ref().map(|g| g.anchor_m))
    {
        local_geometry::clip(faces, plane, anchor)
    } else {
        clip_legacy(faces, plane, eps)
    }
}
fn clip_legacy<S: Scalar>(
    faces: &[CutPolygon<S>],
    plane: &Plane<S>,
    eps: f64,
) -> CaeResult<Vec<CutPolygon<S>>> {
    let mut out = Vec::new();
    let mut cap = Vec::new();
    let mut has_inside = false;
    let mut has_outside = false;
    for face in faces {
        if face.support_only {
            out.push(face.clone());
            continue;
        }
        for &p in &face.vertices_m {
            let d = distance(plane, p, eps).value();
            has_inside |= d > 0.;
            has_outside |= d < 0.;
        }
        let polygon = clip_polygon(&face.vertices_m, plane, eps)?;
        for &p in &polygon {
            if distance(plane, p, eps).value() == 0.
                && cap.iter().all(|&q| norm(sub(primal(p), primal(q))) > eps)
            {
                cap.push(p);
            }
        }
        if polygon.len() >= 3 && area(&polygon).value() > 0. {
            out.push(CutPolygon {
                local_geometry: None,
                support_only: false,
                vertices_m: polygon,
                outward_normal: face.outward_normal,
                plane_origin_m: face.plane_origin_m,
                plane_offset_m: face.plane_offset_m,
                boundary: face.boundary.clone(),
            });
        } else {
            let mut support = face.clone();
            support.support_only = true;
            support.vertices_m.clear();
            out.push(support);
        }
    }
    let mut cap_added = false;
    if has_inside && has_outside && cap.len() >= 3 {
        let centre: [f64; 3] = std::array::from_fn(|k| {
            cap.iter().map(|p| p[k].value()).sum::<f64>() / cap.len() as f64
        });
        let n = primal(plane.normal);
        let axis = if n[0].abs() < 0.8 {
            [1., 0., 0.]
        } else {
            [0., 1., 0.]
        };
        let u = cross(n, axis);
        let u = u.map(|x| x / norm(u));
        let v = cross(n, u);
        cap.sort_by(|a, b| {
            let pa = sub(primal(*a), centre);
            let pb = sub(primal(*b), centre);
            dot(pa, v)
                .atan2(dot(pa, u))
                .total_cmp(&dot(pb, v).atan2(dot(pb, u)))
        });
        if area(&cap).value() > 0. {
            cap_added = true;
            out.push(CutPolygon {
                local_geometry: None,
                support_only: false,
                vertices_m: cap,
                outward_normal: plane.normal.map(|x| -x),
                plane_origin_m: plane.origin,
                plane_offset_m: -plane.offset,
                boundary: plane.boundary.clone(),
            });
        }
    }
    if !cap_added {
        out.push(CutPolygon {
            local_geometry: None,
            support_only: true,
            vertices_m: Vec::new(),
            outward_normal: plane.normal.map(|x| -x),
            plane_origin_m: plane.origin,
            plane_offset_m: -plane.offset,
            boundary: plane.boundary.clone(),
        });
    }
    Ok(out)
}
pub(crate) fn measure_in_frame<S: TransientGasScalar>(faces:&[CutPolygon<S>], anchor:[f64;3])->CaeResult<(S,[S;3])> {
    if !anchor.iter().all(|v|v.is_finite()) {return Err(fail("cut measure frame is nonfinite"));}
    if local_geometry::unique_volume_vertices(faces,anchor)?<4 || local_geometry::maintained_native_plane_rank_zero(faces)? {
        return Ok((S::zero(),[S::zero();3]));
    }
    let local=faces.iter().map(|face|local_geometry::view_in_frame(face,anchor)).collect::<Vec<_>>();
    measure_legacy(&local)
}
pub(crate) fn measure<S: TransientGasScalar>(faces: &[CutPolygon<S>]) -> CaeResult<(S, [S; 3])> {
    if let Some(anchor) = faces
        .iter()
        .find_map(|face| face.local_geometry.as_ref().map(|g| g.anchor_m))
    {
        if local_geometry::unique_volume_vertices(faces, anchor)? < 4
            || local_geometry::maintained_native_plane_rank_zero(faces)?
        {
            return Ok((S::zero(), [S::zero(); 3]));
        }
        let local = faces
            .iter()
            .map(|face| local_geometry::view_in_frame(face, anchor))
            .collect::<Vec<_>>();
        let (v, m) = measure_legacy(&local)?;
        Ok((v, std::array::from_fn(|k| m[k] + v * anchor[k])))
    } else {
        measure_legacy(faces)
    }
}
fn measure_legacy<S: Scalar>(faces: &[CutPolygon<S>]) -> CaeResult<(S, [S; 3])> {
    let count: usize = faces
        .iter()
        .filter(|f| !f.support_only)
        .map(|f| f.vertices_m.len())
        .fold(0usize, |a, b| a + b);
    if count == 0 {
        return Ok((S::zero(), [S::zero(); 3]));
    }
    let centre: [S; 3] = std::array::from_fn(|k| {
        faces
            .iter()
            .filter(|f| !f.support_only)
            .flat_map(|f| &f.vertices_m)
            .map(|p| p[k])
            .fold(S::zero(), |a, b| a + b)
            / count as f64
    });
    let mut vol = S::zero();
    let mut moment = [S::zero(); 3];
    for f in faces {
        if f.support_only {
            continue;
        }
        for i in 1..f.vertices_m.len().saturating_sub(1) {
            let points = [
                centre,
                f.vertices_m[0],
                f.vertices_m[i],
                f.vertices_m[i + 1],
            ];
            let dv = dot(
                sub(points[1], centre),
                cross(sub(points[2], centre), sub(points[3], centre)),
            )
            .abs()
                / 6.;
            vol += dv;
            for k in 0..3 {
                moment[k] +=
                    dv * (points[0][k] + points[1][k] + points[2][k] + points[3][k]) * 0.25;
            }
        }
    }
    if !vol.value().is_finite() {
        return Err(fail("cut-cell volume overflow"));
    }
    Ok((vol, moment))
}
pub(crate) fn central_second_integral<S: Scalar>(
    parts: &[Vec<CutPolygon<S>>],
    anchor: [S; 3],
) -> CaeResult<[[S; 3]; 3]> {
    if let Some(frame) = parts
        .iter()
        .flatten()
        .find_map(|face| face.local_geometry.as_ref().map(|g| g.anchor_m))
    {
        let local = parts
            .iter()
            .map(|faces| {
                faces
                    .iter()
                    .map(|face| local_geometry::view_in_frame(face, frame))
                    .collect()
            })
            .collect::<Vec<Vec<_>>>();
        central_second_integral_legacy(&local, std::array::from_fn(|k| anchor[k] - frame[k]))
    } else {
        central_second_integral_legacy(parts, anchor)
    }
}
pub(crate) fn centroid_second_integral<S: Scalar>(
    parts: &[Vec<CutPolygon<S>>],
    world_centroid: [S; 3],
) -> CaeResult<[[S; 3]; 3]> {
    let Some(frame) = parts
        .iter()
        .flatten()
        .find_map(|face| face.local_geometry.as_ref().map(|g| g.anchor_m))
    else {
        return central_second_integral(parts, world_centroid);
    };
    let local = parts
        .iter()
        .map(|faces| {
            faces
                .iter()
                .map(|face| local_geometry::view_in_frame(face, frame))
                .collect()
        })
        .collect::<Vec<Vec<_>>>();
    let mut volume = S::zero();
    let mut moment = [S::zero(); 3];
    for faces in &local {
        let (v, m) = measure_legacy(faces)?;
        volume += v;
        for k in 0..3 {
            moment[k] += m[k];
        }
    }
    if !volume.value().is_finite() || volume.value() <= 0. {
        return Err(fail("cut-cell centroid volume must be positive"));
    }
    let centroid = moment.map(|m| m / volume);
    if centroid.iter().any(|v| !v.value().is_finite()) {
        return Err(fail("cut-cell centroid overflow"));
    }
    central_second_integral_legacy(&local, centroid)
}
fn has_retained_geometry<S: Scalar>(faces: &[CutPolygon<S>]) -> bool {
    faces.iter().any(|face| face.local_geometry.is_some())
}
fn central_second_integral_legacy<S: Scalar>(
    parts: &[Vec<CutPolygon<S>>],
    anchor: [S; 3],
) -> CaeResult<[[S; 3]; 3]> {
    let mut integral = [[S::zero(); 3]; 3];
    for faces in parts {
        let count: usize = faces
            .iter()
            .filter(|f| !f.support_only)
            .map(|f| f.vertices_m.len())
            .fold(0usize, |a, b| a + b);
        if count == 0 {
            continue;
        }
        let centre: [S; 3] = std::array::from_fn(|k| {
            faces
                .iter()
                .filter(|f| !f.support_only)
                .flat_map(|f| &f.vertices_m)
                .map(|p| p[k])
                .fold(S::zero(), |a, b| a + b)
                / count as f64
        });
        for face in faces.iter().filter(|f| !f.support_only) {
            for i in 1..face.vertices_m.len().saturating_sub(1) {
                let points = [
                    centre,
                    face.vertices_m[0],
                    face.vertices_m[i],
                    face.vertices_m[i + 1],
                ];
                let dv = dot(
                    sub(points[1], centre),
                    cross(sub(points[2], centre), sub(points[3], centre)),
                )
                .abs()
                    / 6.;
                let r = points.map(|p| sub(p, anchor));
                let sum: [S; 3] =
                    std::array::from_fn(|k| r.iter().map(|p| p[k]).fold(S::zero(), |a, b| a + b));
                for j in 0..3 {
                    for k in j..3 {
                        integral[j][k] += dv
                            * (sum[j] * sum[k]
                                + r.iter().map(|p| p[j] * p[k]).fold(S::zero(), |a, b| a + b))
                            / 20.;
                    }
                }
            }
        }
    }
    for j in 0..3 {
        for k in j + 1..3 {
            integral[k][j] = integral[j][k];
        }
    }
    if integral.iter().flatten().any(|v| !v.value().is_finite()) {
        return Err(fail("cut-cell second moment overflow"));
    }
    Ok(integral)
}
pub(crate) fn triangle_moment_rule() -> ([[f64; 3]; 6], [f64; 6]) {
    let a = (3f64 / 5.).sqrt();
    let u = [0.5 * (1. - a), 0.5, 0.5 * (1. + a)];
    let wu = [5. / 18., 4. / 9., 5. / 18.];
    let b = 1. / 3f64.sqrt();
    let v = [0.5 * (1. - b), 0.5 * (1. + b)];
    let bary = std::array::from_fn(|i| {
        [
            (1. - u[i / 2]) * (1. - v[i % 2]),
            (1. - u[i / 2]) * v[i % 2],
            u[i / 2],
        ]
    });
    let weights = std::array::from_fn(|i| wu[i / 2] * (1. - u[i / 2]));
    (bary, weights)
}
pub(crate) fn native_planes<S: TransientGasScalar>(
    p: [[S; 3]; 4],
    rho: [S; 4],
    e: [usize; 4],
    body: usize,
    element: usize,
    exterior: &BTreeSet<[usize; 3]>,
    iso: f64,
) -> CaeResult<Vec<Plane<S>>> {
    let mut planes = Vec::new();
    for f in [[0, 1, 2, 3], [0, 1, 3, 2], [0, 2, 3, 1], [1, 2, 3, 0]] {
        let mut indices = [f[0], f[1], f[2]];
        indices.sort_by_key(|&i| e[i]);
        let origin = p[indices[0]];
        let mut n = unit(cross(
            sub(p[indices[1]], origin),
            sub(p[indices[2]], origin),
        ))?;
        if dot(n, sub(p[f[3]], origin)).value() < 0. {
            n = n.map(|x| -x);
        }
        let face = indices.map(|i| e[i]);
        let physical =
            exterior.contains(&face) || [f[0], f[1], f[2]].iter().all(|&i| rho[i].value() == iso);
        planes.push(Plane { native_levelset:None, subdomain_face:None, native_face: Some(NativePlaneFace::new(body, face, indices.map(|i|p[i]), n, S::zero())),
            local_origin: None,
            normal: n,
            origin,
            offset: S::zero(),
            boundary: CutBoundary::Native {
                body,
                element,
                face: Some(face),
                physical,
            },
        });
    }
    if rho.iter().any(|r| r.value() < iso) {
        let edges = [sub(p[1], p[0]), sub(p[2], p[0]), sub(p[3], p[0])];
        let cof = [
            cross(edges[1], edges[2]),
            cross(edges[2], edges[0]),
            cross(edges[0], edges[1]),
        ];
        let det = dot(edges[0], cof[0]);
        let gradient: [S; 3] = std::array::from_fn(|a| {
            ((rho[1] - rho[0]) * cof[0][a]
                + (rho[2] - rho[0]) * cof[1][a]
                + (rho[3] - rho[0]) * cof[2][a])
                / det
        });
        let length = dot(gradient, gradient).sqrt();
        let normal=gradient.map(|x| x / length);
        let offset=(rho[0]-iso)/length;
        planes.push(Plane { native_levelset:Some(NativeLevelsetPlane::new(body,element,e,p,rho,iso,normal,offset)), subdomain_face:None, native_face: None,
            local_origin: None,
            normal,
            origin: p[0],
            offset,
            boundary: CutBoundary::Native {
                body,
                element,
                face: None,
                physical: true,
            },
        });
    }
    Ok(planes)
}
pub(crate) fn native_planes_from_source<S:TransientGasScalar>(source:std::sync::Arc<crate::native_geometry_source::NativeElementSource<S>>,rho:[S;4],exterior:&BTreeSet<[usize;3]>,iso:f64,anchor:[f64;3])->CaeResult<Vec<Plane<S>>> {
    source.validate()?;
    let p=source.vertices_in_frame(anchor)?;
    let e=source.nodes;let body=source.body;let element=source.element;
    let mut planes=Vec::new();
    for f in [[0,1,2,3],[0,1,3,2],[0,2,3,1],[1,2,3,0]] {
        let mut indices=[f[0],f[1],f[2]];indices.sort_by_key(|i|e[*i]);
        let mut normal=source.face_normal(indices)?;
        let opposite=source.points[f[3]].edge_from(&source.points[indices[0]])?;
        if dot(normal,opposite).value()<0.{normal=normal.map(|v|-v);}
        let face=indices.map(|i|e[i]);let physical=exterior.contains(&face) || [f[0],f[1],f[2]].iter().all(|i|rho[*i].value()==iso);
        let mut receipt=NativePlaneFace::new(body,face,indices.map(|i|p[i]),normal,S::zero());receipt.set_anchor(anchor);receipt.set_source(source.clone());
        let origin=p[indices[0]];
        planes.push(Plane{native_face:Some(receipt),native_levelset:None,subdomain_face:None,normal,origin:std::array::from_fn(|k|origin[k]+anchor[k]),offset:S::zero(),local_origin:Some((anchor,origin)),boundary:CutBoundary::Native{body,element,face:Some(face),physical}});
    }
    if rho.iter().any(|r|r.value()<iso) {
        let (normal,offset)=source.levelset(rho,iso)?;
        let mut receipt=NativeLevelsetPlane::new(body,element,e,p,rho,iso,normal,offset);receipt.set_anchor(anchor);receipt.set_source(source);
        planes.push(Plane{native_face:None,native_levelset:Some(receipt),subdomain_face:None,normal,origin:std::array::from_fn(|k|p[0][k]+anchor[k]),offset,local_origin:Some((anchor,p[0])),boundary:CutBoundary::Native{body,element,face:None,physical:true}});
    }
    Ok(planes)
}
fn box_poly<S: Scalar>(b: [[f64; 3]; 2]) -> Vec<CutPolygon<S>> {
    let p: [[S; 3]; 8] =
        std::array::from_fn(|i| std::array::from_fn(|a| S::from_f64(b[(i >> a) & 1][a])));
    [
        [0, 2, 6, 4],
        [1, 3, 7, 5],
        [0, 1, 5, 4],
        [2, 3, 7, 6],
        [0, 1, 3, 2],
        [4, 5, 7, 6],
    ]
    .into_iter()
    .enumerate()
    .map(|(i, f)| {
        let mut n = [S::zero(); 3];
        n[i / 2] = S::from_f64(if i % 2 == 0 { -1. } else { 1. });
        CutPolygon {
            local_geometry: None,
            support_only: false,
            vertices_m: f.map(|j| p[j]).to_vec(),
            outward_normal: n,
            plane_origin_m: p[f[0]],
            plane_offset_m: S::zero(),
            boundary: CutBoundary::Cell {
                axis: i / 2,
                side: i % 2,
            },
        }
    })
    .collect()
}
fn local_box_poly<S: TransientGasScalar>(bounds: [[f64; 3]; 2]) -> CaeResult<Vec<CutPolygon<S>>> {
    let anchor = bounds[0];
    let mut faces: Vec<CutPolygon<S>> = box_poly(bounds);
    for face in &mut faces {
        bind_polygon_frame(face, anchor)?;
    }
    let planes = faces
        .iter()
        .map(|face| Plane { native_levelset:None, subdomain_face:None, native_face: None,
            normal: face.outward_normal.map(|v| -v),
            origin: face.plane_origin_m,
            local_origin: face
                .local_geometry
                .as_ref()
                .map(|g| (g.anchor_m, g.plane_origin_m)),
            offset: S::zero(),
            boundary: face.boundary.clone(),
        })
        .collect::<Vec<_>>();
    for face in &mut faces {
        for vertex in 0..face.vertices_m.len() {
            for plane in &planes {
                let CutBoundary::Cell { axis, .. } = plane.boundary else {
                    continue;
                };
                if face.vertices_m[vertex][axis].value() == plane.origin[axis].value() {
                    local_geometry::seed_vertex_plane(face, vertex, plane)?;
                }
            }
        }
    }
    Ok(faces)
}
fn owned_cell_box_poly<S: TransientGasScalar>(owner:&NativeGapColumnOwner,slot:usize)->CaeResult<Vec<CutPolygon<S>>> {
    let anchor=owner.bounds.get(slot).ok_or_else(||CaeError::contract("cut cell box slot is absent"))?[0];
    let cell=*owner.grid.cells.get(slot).ok_or_else(||CaeError::contract("cut cell box original owner is absent"))?;
    let bounds=owner.grid.cell_bounds_in_frame(cell,anchor)?;
    let mut faces=box_poly::<S>(bounds);
    for face in &mut faces {
        bind_polygon_frame(face,[0.;3])?;
        let local=face.local_geometry.as_mut().ok_or_else(||CaeError::contract("cut cell box local authority is absent"))?;
        local.anchor_m=anchor;
        face.vertices_m=local.vertices_m.iter().map(|p|std::array::from_fn(|k|p[k]+S::from_f64(anchor[k]))).collect();
        face.plane_origin_m=std::array::from_fn(|k|local.plane_origin_m[k]+S::from_f64(anchor[k]));
    }
    let planes=faces.iter().map(|face|Plane { native_levelset:None, subdomain_face:None,native_face:None,normal:face.outward_normal.map(|v|-v),origin:face.plane_origin_m,local_origin:face.local_geometry.as_ref().map(|g|(g.anchor_m,g.plane_origin_m)),offset:S::zero(),boundary:face.boundary.clone()}).collect::<Vec<_>>();
    for face in &mut faces {
        let points=polygon_vertices_in_frame(face,anchor);
        for (vertex,point) in points.iter().enumerate() {
            for plane in &planes {
                let CutBoundary::Cell{axis,..}=plane.boundary else{continue;};
                let origin=plane.local_origin.ok_or_else(||CaeError::contract("cut cell box plane authority is absent"))?.1;
                if point[axis].value()==origin[axis].value(){local_geometry::seed_vertex_plane(face,vertex,plane)?;}
            }
        }
    }
    Ok(faces)
}

pub(crate) fn subtract<S: TransientGasScalar>(
    poly: Vec<CutPolygon<S>>,
    planes: &[Plane<S>],
    eps: f64,
    _volume_error: f64,
) -> CaeResult<Vec<Vec<CutPolygon<S>>>> {
    let mut remaining = poly;
    let mut out = Vec::new();
    for plane in planes {
        let reversed = Plane { native_levelset:plane.native_levelset.clone(), subdomain_face:plane.subdomain_face.clone(), native_face: plane.native_face.clone(),
            local_origin: plane.local_origin,
            normal: plane.normal.map(|x| -x),
            origin: plane.origin,
            offset: -plane.offset,
            boundary: plane.boundary.clone(),
        };
        let exterior = clip(&remaining, &reversed, eps)?;
        let (v, _) = measure(&exterior)?;
        let points: Vec<_> = exterior
            .iter()
            .flat_map(|f| &f.vertices_m)
            .copied()
            .collect();
        if v.value() > 0. && (has_retained_geometry(&exterior) || !dimensionless_zero(&points, eps))
        {
            out.push(exterior);
        }
        remaining = clip(&remaining, plane, eps)?;
        let (v, _) = measure(&remaining)?;
        let points: Vec<_> = remaining
            .iter()
            .flat_map(|f| &f.vertices_m)
            .copied()
            .collect();
        if v.value() == 0.
            || (!has_retained_geometry(&remaining) && dimensionless_zero(&points, eps))
        {
            break;
        }
    }
    Ok(out)
}
#[derive(Clone, Copy, Debug)]
struct Wide(f64, f64);
impl Wide {
    fn add(self, b: Self) -> Self {
        let h = self.0 + b.0;
        let z = h - self.0;
        let e = (self.0 - (h - z)) + (b.0 - z) + self.1 + b.1;
        let r = h + e;
        Self(r, e - (r - h))
    }
    fn neg(self) -> Self {
        Self(-self.0, -self.1)
    }
    fn mul(self, b: Self) -> Self {
        let h = self.0 * b.0;
        let e = self.0.mul_add(b.0, -h) + self.0 * b.1 + self.1 * b.0 + self.1 * b.1;
        let r = h + e;
        Self(r, e - (r - h))
    }
    fn value(self) -> f64 {
        self.0 + self.1
    }
}
fn difference(a: f64, b: f64) -> Wide {
    Wide(a, 0.).add(Wide(-b, 0.))
}
fn wide_cross(a: [Wide; 3], b: [Wide; 3]) -> [Wide; 3] {
    [
        a[1].mul(b[2]).add(a[2].mul(b[1]).neg()),
        a[2].mul(b[0]).add(a[0].mul(b[2]).neg()),
        a[0].mul(b[1]).add(a[1].mul(b[0]).neg()),
    ]
}
fn wide_norm(v: [Wide; 3]) -> f64 {
    v.iter().map(|x| x.value().powi(2)).sum::<f64>().sqrt()
}
pub(crate) fn dimensionless_zero<S: Scalar>(points: &[[S; 3]], eps: f64) -> bool {
    if points.len() < 4 {
        return true;
    }
    let p0 = primal(points[0]);
    let vectors: Vec<[Wide; 3]> = points
        .iter()
        .map(|p| std::array::from_fn(|k| difference(p[k].value(), p0[k])))
        .collect();
    let edge = *vectors
        .iter()
        .max_by(|a, b| wide_norm(**a).total_cmp(&wide_norm(**b)))
        .unwrap();
    let length = wide_norm(edge);
    if length <= eps {
        return true;
    }
    let normal = vectors
        .iter()
        .map(|&v| wide_cross(edge, v))
        .max_by(|a, b| wide_norm(*a).total_cmp(&wide_norm(*b)))
        .unwrap();
    let scale = wide_norm(normal);
    if scale <= eps * length {
        return true;
    }
    vectors.iter().all(|v| {
        (0..3)
            .fold(Wide(0., 0.), |sum, k| sum.add(normal[k].mul(v[k])))
            .value()
            .abs()
            <= eps * scale
    })
}

fn same_support_plane<S: Scalar>(
    a: &CutPolygon<S>,
    b: &CutPolygon<S>,
    eps: f64,
) -> CaeResult<bool> {
    let na = primal(a.outward_normal);
    let nb = primal(b.outward_normal);
    let la = norm(na);
    let lb = norm(nb);
    if !la.is_finite() || !lb.is_finite() || la == 0. || lb == 0. {
        return Err(fail("invalid facet supporting normal"));
    }
    let angular = norm(cross(na, nb));
    let roundoff = 128. * f64::EPSILON * la * lb;
    if !angular.is_finite() || !roundoff.is_finite() {
        return Err(fail("facet supporting normal overflow"));
    }
    if angular > roundoff {
        return Ok(false);
    }
    let separation = (dot(a.outward_normal, sub(b.plane_origin_m, a.plane_origin_m))
        + a.plane_offset_m
        - dot(a.outward_normal, b.outward_normal) * b.plane_offset_m
            / dot(b.outward_normal, b.outward_normal))
    .value()
    .abs();
    if !separation.is_finite() {
        return Err(fail("facet supporting offset overflow"));
    }
    Ok(separation <= eps * la)
}
pub(crate) fn polygon_part_intersection<S: Scalar>(
    a: &CutPolygon<S>,
    b: &CutPolygon<S>,
    part: &[CutPolygon<S>],
    eps: f64,
) -> CaeResult<Vec<[S; 3]>> {
    if a.support_only || b.support_only || a.vertices_m.len() < 3 || b.vertices_m.len() < 3 {
        return Ok(Vec::new());
    }
    if !same_support_plane(a, b, eps)? {
        return Ok(Vec::new());
    }
    if b.vertices_m.iter().any(|&p| {
        (dot(a.outward_normal, sub(p, a.plane_origin_m)) + a.plane_offset_m)
            .value()
            .abs()
            > eps
    }) {
        return Ok(Vec::new());
    }
    if (0..3).any(|k| {
        let al = a
            .vertices_m
            .iter()
            .map(|p| p[k].value())
            .fold(f64::INFINITY, f64::min);
        let ah = a
            .vertices_m
            .iter()
            .map(|p| p[k].value())
            .fold(f64::NEG_INFINITY, f64::max);
        let bl = b
            .vertices_m
            .iter()
            .map(|p| p[k].value())
            .fold(f64::INFINITY, f64::min);
        let bh = b
            .vertices_m
            .iter()
            .map(|p| p[k].value())
            .fold(f64::NEG_INFINITY, f64::max);
        ah < bl - eps || bh < al - eps
    }) {
        return Ok(Vec::new());
    }
    let mut polygon = a.vertices_m.clone();
    for face in part {
        let origin = face.plane_origin_m;
        polygon = clip_polygon(
            &polygon,
            &Plane { native_levelset:None, subdomain_face:None, native_face: None,
                local_origin: None,
                normal: face.outward_normal.map(|v| -v),
                origin,
                offset: -face.plane_offset_m,
                boundary: face.boundary.clone(),
            },
            eps,
        )?;
        if polygon.len() < 3 {
            break;
        }
    }
    Ok(polygon)
}
pub(crate) fn polygon_intersection<S: Scalar>(
    a: &CutPolygon<S>,
    b: &CutPolygon<S>,
    eps: f64,
) -> CaeResult<Vec<[S; 3]>> {
    if a.support_only || b.support_only || a.vertices_m.len() < 3 || b.vertices_m.len() < 3 {
        return Ok(Vec::new());
    }
    if (0..3).any(|k| {
        let al = a
            .vertices_m
            .iter()
            .map(|p| p[k].value())
            .fold(f64::INFINITY, f64::min);
        let ah = a
            .vertices_m
            .iter()
            .map(|p| p[k].value())
            .fold(f64::NEG_INFINITY, f64::max);
        let bl = b
            .vertices_m
            .iter()
            .map(|p| p[k].value())
            .fold(f64::INFINITY, f64::min);
        let bh = b
            .vertices_m
            .iter()
            .map(|p| p[k].value())
            .fold(f64::NEG_INFINITY, f64::max);
        ah < bl - eps || bh < al - eps
    }) {
        return Ok(Vec::new());
    }
    if !same_support_plane(a, b, eps)? {
        return Ok(Vec::new());
    }
    if b.vertices_m.iter().any(|&p| {
        (dot(a.outward_normal, sub(p, a.plane_origin_m)) + a.plane_offset_m)
            .value()
            .abs()
            > eps
    }) {
        return Ok(Vec::new());
    }
    let centre: [S; 3] = std::array::from_fn(|k| {
        b.vertices_m
            .iter()
            .map(|p| p[k])
            .fold(S::zero(), |a, b| a + b)
            / b.vertices_m.len() as f64
    });
    let mut poly = a.vertices_m.clone();
    for i in 0..b.vertices_m.len() {
        let origin = b.vertices_m[i];
        let edge = sub(b.vertices_m[(i + 1) % b.vertices_m.len()], origin);
        if norm(primal(edge)) <= eps {
            continue;
        }
        let mut normal = unit(cross(b.outward_normal, edge))?;
        if dot(normal, sub(centre, origin)).value() < 0. {
            normal = normal.map(|x| -x);
        }
        poly = clip_polygon(
            &poly,
            &Plane { native_levelset:None, subdomain_face:None, native_face: None,
                local_origin: None,
                normal,
                origin,
                offset: S::zero(),
                boundary: b.boundary.clone(),
            },
            eps,
        )?;
        if poly.len() < 3 {
            break;
        }
    }
    Ok(poly)
}
pub(crate) fn polygon_intersection_local<S: TransientGasScalar>(
    a: &CutPolygon<S>,
    b: &CutPolygon<S>,
    eps: f64,
) -> CaeResult<CutPolygon<S>> {
    let anchor = a
        .local_geometry
        .as_ref()
        .or(b.local_geometry.as_ref())
        .map_or([0.; 3], |g| g.anchor_m);
    let mut polygon = a.clone();
    bind_polygon_frame(&mut polygon, anchor)?;
    let av = local_geometry::view_in_frame(a, anchor);
    let bv = local_geometry::view_in_frame(b, anchor);
    if a.support_only
        || b.support_only
        || !local_positive_polygon(&av)
        || !local_positive_polygon(&bv)
        || !local_geometry::exact_same_support(a, b, anchor)
    {
        polygon.support_only = true;
        polygon.vertices_m.clear();
        if let Some(g) = polygon.local_geometry.as_mut() {
            g.vertices_m.clear();
            g.incidences.clear();
        }
        return Ok(polygon);
    }
    let points = polygon_vertices_in_frame(b, anchor);
    let centre: [S; 3] = std::array::from_fn(|k| {
        points.iter().map(|p| p[k]).fold(S::zero(), |a, b| a + b) / points.len() as f64
    });
    for i in 0..points.len() {
        let owned_plane=match local_geometry::classify_owned_edge(b,i,anchor)? {
            local_geometry::OwnedEdgeOutcome::Plane(plane)=>Some(plane),
            local_geometry::OwnedEdgeOutcome::Zero=>continue,
            local_geometry::OwnedEdgeOutcome::Unproved=>None,
        };
        let origin = points[i];
        let edge = sub(points[(i + 1) % points.len()], origin);
        if edge.iter().all(|v| {
            v.value() == 0.
                && v.geometry_first_order_directions()
                    .is_some_and(|d| d.iter().all(|v| *v == 0.))
        }) {
            continue;
        }
        let plane=if let Some(plane)=owned_plane {plane} else {
        let mut normal = unit(cross(b.outward_normal, edge))?;
        if dot(normal, sub(centre, origin)).value() < 0. {
            normal = normal.map(|v| -v);
        }
        let derived_plane = Plane { native_levelset:None, subdomain_face:None, native_face: None,
            normal,
            origin: std::array::from_fn(|k| origin[k] + anchor[k]),
            local_origin: Some((anchor, origin)),
            offset: S::zero(),
            boundary: b.boundary.clone(),
        };
        derived_plane
        };
        polygon = clip(&[polygon], &plane, eps)?
            .into_iter()
            .next()
            .ok_or_else(|| fail("cut local polygon result is absent"))?;
        if polygon.support_only {
            break;
        }
    }
    Ok(polygon)
}
pub(crate) fn polygon_part_intersection_local<S: TransientGasScalar>(
    a: &CutPolygon<S>,
    b: &CutPolygon<S>,
    part: &[CutPolygon<S>],
    eps: f64,
) -> CaeResult<CutPolygon<S>> {
    let mut polygon = polygon_intersection_local(a, b, eps)?;
    for face in part {
        if polygon.support_only {
            break;
        }
        let local_origin = face
            .local_geometry
            .as_ref()
            .map(|g| (g.anchor_m, g.plane_origin_m));
        let plane = Plane { native_levelset:local_geometry::native_levelset_for_polygon(face), subdomain_face:local_geometry::subdomain_face_for_polygon(face), native_face: local_geometry::native_face_for_polygon(face),
            normal: face.outward_normal.map(|v| -v),
            origin: face.plane_origin_m,
            local_origin,
            offset: -face.plane_offset_m,
            boundary: face.boundary.clone(),
        };
        polygon = clip(&[polygon], &plane, eps)?
            .into_iter()
            .next()
            .ok_or_else(|| fail("cut local polygon result is absent"))?;
    }
    Ok(polygon)
}
fn root(parent: &mut [usize], i: usize) -> usize {
    if parent[i] != i {
        parent[i] = root(parent, parent[i]);
    }
    parent[i]
}
impl NativeCutCellOwner {
    pub fn from_native(
        binding: &NativeNodalBinding,
        grid: NativeGapColumnGrid,
        iso: f64,
        tolerance_m: f64,
    ) -> CaeResult<Self> {
        Ok(Self {
            capacity: NativeGapColumnOwner::from_native(binding, grid, iso, tolerance_m)?,
        })
    }
    pub fn from_capacity_owner(capacity: NativeGapColumnOwner) -> Self {
        Self { capacity }
    }
    pub fn capacity_owner(&self) -> &NativeGapColumnOwner {
        &self.capacity
    }
    pub fn certify_full_fluid_faces<S: Scalar>(
        &self,
        partition: &NativeCutCellPartition<S>,
        faces: &[(usize, usize, usize)],
    ) -> CaeResult<()> {
        let mut selected = BTreeSet::new();
        for &(cell, axis, side) in faces {
            if axis >= 3 || side >= 2 || !selected.insert((cell, axis, side)) {
                return Err(fail("invalid duplicate native fluid interface face"));
            }
            let bounds = self.capacity.grid.cell_bounds(cell)?;
            if !self.capacity.grid.cells.contains(&cell) {
                return Err(fail("fluid interface cell is not owned"));
            }
            let height = bounds[side][axis];
            let expected = self.capacity.grid.spacing_m.powi(2);
            let mut actual = 0.;
            for aperture in &partition.apertures {
                if aperture.right.is_none()
                    && partition.components[aperture.left].cell == cell
                    && aperture.axis == Some(axis)
                    && (aperture.normal[axis].value() > 0.) == (side == 1)
                {
                    actual += aperture.area_m2.value();
                }
            }
            let bound = 256. * f64::EPSILON * expected
                + 8. * partition.arithmetic_length_m * self.capacity.grid.spacing_m;
            if !expected.is_finite()
                || expected <= 0.
                || !bound.is_finite()
                || !actual.is_finite()
                || (actual - expected).abs() > bound
            {
                return Err(fail("native bulk interface is not wholly fluid"));
            }
            for wall in &partition.walls {
                if wall.vertices_m.iter().any(|p| {
                    (p[axis].value() - height).abs() <= partition.arithmetic_length_m
                        && (0..3).filter(|&k| k != axis).all(|k| {
                            p[k].value() >= bounds[0][k] - partition.arithmetic_length_m
                                && p[k].value() <= bounds[1][k] + partition.arithmetic_length_m
                        })
                }) {
                    return Err(fail("native solid touches bulk interface"));
                }
            }
        }
        Ok(())
    }
    pub fn partition_jvp(
        &self,
        trace: &[f64],
        design: &[f64],
        trace_direction: &[f64],
        design_direction: &[f64],
    ) -> CaeResult<NativeCutCellPartition<Dual<1>>> {
        self.capacity
            .capacity_jvp(trace, design, trace_direction, design_direction)?;
        let t: Vec<_> = trace
            .iter()
            .zip(trace_direction)
            .map(|(&v, &d)| Dual::new(v, [d]))
            .collect();
        let x: Vec<_> = design
            .iter()
            .zip(design_direction)
            .map(|(&v, &d)| Dual::new(v, [d]))
            .collect();
        self.partition(&t, &x)
    }
    pub fn partition<S: TransientGasScalar>(
        &self,
        trace: &[S],
        design: &[S],
    ) -> CaeResult<NativeCutCellPartition<S>> {
        let capacity = self.capacity.evaluate(trace, design)?;
        let owner = &self.capacity;
        let scale = owner
            .bounds
            .iter()
            .flatten()
            .flatten()
            .map(|x| x.abs())
            .fold(owner.grid.spacing_m, f64::max);
        let eps = 128. * f64::EPSILON * scale;

        let mut density = vec![S::zero(); owner.phase.nrows()];
        for (i, r) in density.iter_mut().enumerate() {
            let (c, w) = owner.phase.row(i);
            for (&j, &a) in c.iter().zip(w) {
                *r += design[j] * a;
            }
        }
        let positions: [Vec<[S; 3]>; 2] = std::array::from_fn(|b| {
            owner.nodes[b]
                .iter()
                .map(|n| std::array::from_fn(|a| trace[n.force_flux[a]] + n.reference_m[a]))
                .collect()
        });
        let mut solids: Vec<Vec<Vec<Plane<S>>>> = vec![Vec::new(); capacity.len()];
        let mut physical_surfaces: BTreeMap<usize, Vec<CutPolygon<S>>> = BTreeMap::new();
        for body in 0..2 {
            let offset = if body == 0 { 0 } else { owner.nodes[0].len() };
            let exterior: BTreeSet<_> = owner.boundary_faces[body].iter().copied().collect();
            for (element, e) in owner.elements[body].iter().enumerate() {
                let p = e.map(|i| positions[body][i]);
                let rho = e.map(|i| density[offset + i]);
                if rho.iter().all(|r| r.value() < owner.iso) {
                    continue;
                }
                let low: [S; 3] = std::array::from_fn(|k| {
                    S::from_f64(p.iter().map(|q| q[k].value()).fold(f64::INFINITY, f64::min) - eps)
                });
                let high: [S; 3] = std::array::from_fn(|k| {
                    S::from_f64(
                        p.iter()
                            .map(|q| q[k].value())
                            .fold(f64::NEG_INFINITY, f64::max)
                            + eps,
                    )
                });
                let candidates: Vec<_> = owner
                    .point_candidate_slots([low, high, low, high])?
                    .into_iter()
                    .filter(|&i| !capacity[i].certified_zero_air)
                    .collect();
                if candidates.is_empty() {
                    continue;
                }
                for i in candidates {
                    let anchor = owner.bounds[i][0];
                    if let Some(grids) = owner.reference_grids.as_ref() {
                        for &node in e {
                            let replay = grids[body].node_position(node);
                            if (0..3).any(|k| {
                                replay[k].to_bits()
                                    != owner.nodes[body][node].reference_m[k].to_bits()
                            }) {
                                return Err(fail("cut local native reference producer differs"));
                            }
                        }
                    }
                    let mut local_nodes = [[S::zero(); 3]; 4];
                    for vertex in 0..4 {
                        let node = e[vertex];
                        let reference = owner.node_reference_in_frame(body, node, anchor)?;
                        let binding = &owner.nodes[body][node];
                        local_nodes[vertex] = std::array::from_fn(|axis|
                            trace[binding.force_flux[axis]] + reference[axis]);
                    }
                    let coefficient_source=owner.element_coefficient_source(body,element,trace)?;
                    let mut local_planes = native_planes_from_source(coefficient_source,rho,&exterior,owner.iso,anchor)?;
                    for plane in &mut local_planes {
                        let local = plane.local_origin.ok_or_else(||fail("native coefficient plane frame is absent"))?.1;
                        let vertex = match plane.boundary {
                            CutBoundary::Native {
                                face: Some(face), ..
                            } => e
                                .iter()
                                .position(|node| *node == face[0])
                                .ok_or_else(|| fail("cut local native plane origin is absent"))?,
                            _ => 0,
                        };
                        plane.local_origin = Some((anchor, local));
                        if let Some(receipt)=plane.native_face.as_mut(){receipt.set_anchor(anchor);}
                        if let Some(receipt)=plane.native_levelset.as_mut(){receipt.set_anchor(anchor);}
                        plane.origin = p[vertex];
                    }
                    let planes = &local_planes;
                    let mut native_piece = Vec::new();
                    for (j, f) in [[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]]
                        .iter()
                        .enumerate()
                    {
                        let normal = planes[j].normal.map(|v| -v);
                        let mut indices = *f;
                        if dot(
                            cross(
                                sub(local_nodes[indices[1]], local_nodes[indices[0]]),
                                sub(local_nodes[indices[2]], local_nodes[indices[0]]),
                            ),
                            normal,
                        )
                        .value()
                            < 0.
                        {
                            indices.swap(1, 2);
                        }
                        let mut face = CutPolygon {
                            support_only: false,
                            local_geometry: Some(CutLocalGeometry {
                                plane_incidence:None,
                                anchor_m: anchor,
                                vertices_m: indices.map(|k| local_nodes[k]).to_vec(),
                                plane_origin_m: planes[j]
                                    .local_origin
                                    .ok_or_else(|| fail("cut local plane frame is absent"))?
                                    .1,
                                incidences: vec![Vec::new(); 3],
                                point_constructions: vec![local_geometry::PointConstruction::Leaf;3],
                            }),
                            vertices_m: indices.map(|k| p[k]).to_vec(),
                            outward_normal: normal,
                            plane_origin_m: planes[j].origin,
                            plane_offset_m: -planes[j].offset,
                            boundary: planes[j].boundary.clone(),
                        };
                        for (vertex, &k) in indices.iter().enumerate() {
                            for plane in planes.iter().take(4) {
                                if matches!(plane.boundary,CutBoundary::Native{face:Some(nodes),..}if nodes.contains(&e[k]))
                                {
                                    local_geometry::seed_vertex_plane(&mut face, vertex, plane)?;
                                }
                            }
                        }
                        native_piece.push(face);
                    }
                    if planes.len() == 5 {
                        native_piece = clip(&native_piece, &planes[4], eps)?;
                    }
                    let mut physical_piece = native_piece.clone();
                    let local_bounds = owner.grid.cell_bounds_in_frame(owner.grid.cells[i], anchor)?;
                    for axis in 0..3 {
                        for side in 0..2 {
                            let mut normal = [S::zero(); 3];
                            normal[axis] = S::from_f64(if side == 0 { 1. } else { -1. });
                            let plane = Plane { native_levelset:None, subdomain_face:None, native_face: None,
                                local_origin: Some((anchor, local_bounds[side].map(S::from_f64))),
                                normal,
                                origin: owner.bounds[i][side].map(S::from_f64),
                                offset: S::zero(),
                                boundary: CutBoundary::Cell { axis, side },
                            };
                            physical_piece = clip(&physical_piece, &plane, eps)?;
                        }
                    }
                    for face in physical_piece {
                        if matches!(face.boundary, CutBoundary::Native { physical: true, .. })
                            && local_positive_polygon(&face)
                        {
                            physical_surfaces
                                .entry(capacity[i].cell)
                                .or_default()
                                .push(face);
                        }
                    }
                    let mut clipped = owned_cell_box_poly(owner, i)?;

                    for plane in planes {
                        clipped = clip(&clipped, plane, eps)?;
                    }
                    let (v, _) = measure(&clipped)?;
                    if v.value() > 0.
                        && (has_retained_geometry(&clipped)
                            || !dimensionless_zero(
                                &clipped
                                    .iter()
                                    .flat_map(|f| &f.vertices_m)
                                    .copied()
                                    .collect::<Vec<_>>(),
                                eps,
                            ))
                    {
                        solids[i].push(planes.to_vec());
                    }
                }
            }
        }
        let mut components = Vec::new();
        let mut cell_components: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        let mut max_defect: f64 = 0.;
        for (slot, c) in capacity.iter().enumerate() {
            if c.certified_zero_air {
                continue;
            }
            let mut parts = vec![owned_cell_box_poly(owner, slot)?];
            for planes in &solids[slot] {
                let mut next = Vec::new();
                for p in parts {
                    next.extend(subtract(p, planes, eps, c.roundoff_bound_m3)?);
                }
                parts = next;
                if parts.len() > 16384 {
                    return Err(fail("cut-cell arrangement requires subdivision"));
                }
            }
            let mut parent: Vec<_> = (0..parts.len()).collect();
            for i in 0..parts.len() {
                for j in 0..i {
                    let mut adjacent = false;
                    'faces: for a in parts[i].iter().filter(|f| !f.support_only) {
                        for b in parts[j].iter().filter(|f| !f.support_only) {
                            if dot(a.outward_normal, b.outward_normal).value() > -0.5 {
                                continue;
                            }
                            let poly = polygon_intersection_local(a, b, eps)?;
                            if local_positive_polygon(&poly) {
                                adjacent = true;
                                break 'faces;
                            }
                        }
                    }
                    if adjacent {
                        let a = root(&mut parent, i);
                        let b = root(&mut parent, j);
                        parent[a] = b;
                    }
                }
            }
            let mut groups: BTreeMap<usize, Vec<Vec<CutPolygon<S>>>> = BTreeMap::new();
            for (i, p) in parts.into_iter().enumerate() {
                groups.entry(root(&mut parent, i)).or_default().push(p);
            }
            let mut measured = Vec::new();
            for group in groups.into_values() {
                let mut volume = S::zero();
                let mut moment = [S::zero(); 3];
                for part in &group {
                    let (v, m) = measure(part)?;
                    volume += v;
                    for a in 0..3 {
                        moment[a] += m[a];
                    }
                }
                if volume.value() <= 0.
                    || (!group.iter().any(|part| has_retained_geometry(part))
                        && volume.value() <= c.roundoff_bound_m3)
                {
                    let low: [f64; 3] = std::array::from_fn(|k| {
                        group
                            .iter()
                            .flatten()
                            .flat_map(|f| &f.vertices_m)
                            .map(|p| p[k].value())
                            .fold(f64::INFINITY, f64::min)
                    });
                    let high: [f64; 3] = std::array::from_fn(|k| {
                        group
                            .iter()
                            .flatten()
                            .flat_map(|f| &f.vertices_m)
                            .map(|p| p[k].value())
                            .fold(f64::NEG_INFINITY, f64::max)
                    });
                    return Err(fail(&format!(
                        "fluid component is below arithmetic resolution cell {} volume {:e} parts {} extent {:?}",
                        c.cell,
                        volume.value(),
                        group.len(),
                        sub(high, low)
                    )));
                }
                let centroid = moment.map(|m| m / volume);
                let mut signature: Vec<_> = group
                    .iter()
                    .flatten()
                    .flat_map(|f| polygon_vertices_in_frame(f, owner.bounds[slot][0]))
                    .map(primal)
                    .collect();
                signature.sort_by(|a, b| {
                    (0..3)
                        .map(|k| a[k].total_cmp(&b[k]))
                        .find(|o| !o.is_eq())
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                signature.dedup();
                measured.push((centroid, volume, signature, group));
            }
            measured.sort_by(|a, b| {
                (0..3)
                    .map(|k| a.0[k].value().total_cmp(&b.0[k].value()))
                    .find(|o| !o.is_eq())
                    .unwrap_or_else(|| a.1.value().total_cmp(&b.1.value()))
                    .then_with(|| {
                        a.2.iter()
                            .zip(&b.2)
                            .flat_map(|(x, y)| (0..3).map(move |k| x[k].total_cmp(&y[k])))
                            .find(|o| !o.is_eq())
                            .unwrap_or_else(|| a.2.len().cmp(&b.2.len()))
                    })
            });
            for pair in measured.windows(2) {
                if pair[0].2 == pair[1].2 {
                    return Err(fail("indistinguishable cut-cell components"));
                }
            }
            let mut sum = S::zero();
            for (centroid, volume, _, group) in measured {
                let index = components.len();
                let ids = cell_components.entry(c.cell).or_default();
                let local = ids.len();
                ids.push(index);
                sum += volume;
                let local_centroid_m=if let Some(frame)=group.iter().flatten().find_map(|f|f.local_geometry.as_ref().map(|g|g.anchor_m)){
                    let mut local_volume=S::zero();let mut local_moment=[S::zero();3];
                    for part in &group {
                        let local_faces=part.iter().map(|f|local_geometry::view_in_frame(f,frame)).collect::<Vec<_>>();
                        let (v,m)=measure_legacy(&local_faces)?;local_volume+=v;for k in 0..3{local_moment[k]+=m[k];}
                    }
                    if !local_volume.finite_first_order() || local_volume.value()<=0.{return Err(fail("local component centroid volume must be positive"));}
                    let centre=local_moment.map(|m|m/local_volume);
                    if centre.iter().any(|v|!v.finite_first_order() || !v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|v|v.is_finite()))){return Err(fail("local component centroid has invalid channels"));}
                    Some((frame,centre))
                }else{None};
                components.push(CutCellComponent {
                    cell: c.cell,
                    component: local,
                    volume_m3: volume,
                    centroid_m: centroid,
                    local_centroid_m,
                    central_second_integral_m5: centroid_second_integral(&group, centroid)?,
                    convex_parts: group,
                });
            }
            let defect = (sum.value() - c.fluid_capacity_m3.value()).abs();
            max_defect = max_defect.max(defect);
            if defect > c.roundoff_bound_m3 {
                return Err(fail(&format!("connected cut-cell capacity mismatch at cell {}: measured volume = {} m3, capacity = {} m3, arithmetic bound = {} m3", c.cell, sum.value(), c.fluid_capacity_m3.value(), c.roundoff_bound_m3)));
            }
        }
        components.sort_by(|a, b| a.cell.cmp(&b.cell).then(a.component.cmp(&b.component)));
        cell_components.clear();
        for (i, c) in components.iter().enumerate() {
            cell_components.entry(c.cell).or_default().push(i);
        }
        let selected: BTreeSet<_> = owner.grid.cells.iter().copied().collect();
        let mut apertures = Vec::new();
        let mut walls = Vec::new();
        for (left, comp) in components.iter().enumerate() {
            for (left_part, part) in comp.convex_parts.iter().enumerate() {
                for face in part.iter().filter(|f| !f.support_only) {
                    for surface in physical_surfaces.get(&comp.cell).into_iter().flatten() {
                        if dot(face.outward_normal, surface.outward_normal).value() > -0.5 {
                            continue;
                        }
                        let clipped_polygon = polygon_intersection_local(face, surface, eps)?;
                        if !local_positive_polygon(&clipped_polygon) {
                            continue;
                        }
                        let CutBoundary::Native { body, element, .. } = surface.boundary else {
                            continue;
                        };
                        let polygon = clipped_polygon.vertices_m.clone();
                        let local = clipped_polygon
                            .local_geometry
                            .as_ref()
                            .ok_or_else(|| fail("cut local wall geometry is absent"))?;
                        let nodes = owner.elements[body][element];
                        let frame = local.anchor_m;
                        let mut p = [[S::zero(); 3]; 4];
                        for vertex in 0..4 {
                            let node = nodes[vertex];
                            let binding = &owner.nodes[body][node];
                            let reference = owner.node_reference_in_frame(body, node, frame)?;
                            p[vertex] = std::array::from_fn(|axis|
                                trace[binding.force_flux[axis]] + reference[axis]);
                        }
                        let edges = [sub(p[1], p[0]), sub(p[2], p[0]), sub(p[3], p[0])];
                        let cof = [
                            cross(edges[1], edges[2]),
                            cross(edges[2], edges[0]),
                            cross(edges[0], edges[1]),
                        ];
                        let det = dot(edges[0], cof[0]);
                        let bary = local
                            .vertices_m
                            .iter()
                            .map(|&x| {
                                let v = sub(x, p[0]);
                                let z = [
                                    dot(v, cof[0]) / det,
                                    dot(v, cof[1]) / det,
                                    dot(v, cof[2]) / det,
                                ];
                                [S::one() - z[0] - z[1] - z[2], z[0], z[1], z[2]]
                            })
                            .collect();
                        walls.push(CutCellWall {
                            local_geometry: reference_local_geometry(&clipped_polygon),
                            component: left,
                            convex_part: left_part,
                            body,
                            element,
                            area_m2: local_polygon_area(&clipped_polygon),
                            normal: face.outward_normal,
                            vertices_m: polygon,
                            native_nodes: nodes,
                            native_barycentric: bary,
                        });
                    }
                    match face.boundary.clone() {
                        CutBoundary::Cell { axis, side } => {
                            let shape = owner.grid.shape;
                            let ijk = [
                                comp.cell / (shape[1] * shape[2]),
                                (comp.cell / shape[2]) % shape[1],
                                comp.cell % shape[2],
                            ];
                            let outside = if side == 0 && ijk[axis] == 0
                                || side == 1 && ijk[axis] + 1 == shape[axis]
                            {
                                None
                            } else {
                                let mut target = ijk;
                                target[axis] = if side == 0 {
                                    target[axis] - 1
                                } else {
                                    target[axis] + 1
                                };
                                Some((target[0] * shape[1] + target[1]) * shape[2] + target[2])
                            };
                            if outside.is_some_and(|i| selected.contains(&i)) {
                                if side == 0 {
                                    continue;
                                }
                                let neighbour = outside.unwrap();
                                for &right in cell_components.get(&neighbour).into_iter().flatten()
                                {
                                    for (right_part, target) in
                                        components[right].convex_parts.iter().enumerate()
                                    {
                                        for other in target.iter().filter(|f| !f.support_only) {
                                            if other.boundary
                                                != (CutBoundary::Cell { axis, side: 0 })
                                            {
                                                continue;
                                            }
                                            let polygon = polygon_part_intersection_local(
                                                face, other, target, eps,
                                            )?;
                                            let a = local_polygon_area(&polygon);
                                            if local_positive_polygon(&polygon) {
                                                apertures.push(CutCellAperture {
                                                    local_polygons: reference_local_geometry(
                                                        &polygon,
                                                    )
                                                    .map(|g| vec![g]),
                                                    left,
                                                    right: Some(right),
                                                    outside_cell: None,
                                                    axis: Some(axis),
                                                    normal: face.outward_normal,
                                                    area_m2: a,
                                                    polygons: vec![polygon.vertices_m],
                                                    polygon_parts: vec![(
                                                        left_part,
                                                        Some(right_part),
                                                    )],
                                                });
                                            }
                                        }
                                    }
                                }
                            } else {
                                let a = local_polygon_area(face);
                                if local_positive_polygon(face) {
                                    apertures.push(CutCellAperture {
                                        local_polygons: reference_local_geometry(face)
                                            .map(|g| vec![g]),
                                        left,
                                        right: None,
                                        outside_cell: outside,
                                        axis: Some(axis),
                                        normal: face.outward_normal,
                                        area_m2: a,
                                        polygons: vec![face.vertices_m.clone()],
                                        polygon_parts: vec![(left_part, None)],
                                    });
                                }
                            }
                        }
                        CutBoundary::Subdomain { .. } => {}
                        CutBoundary::Native { .. } => {}
                    }
                }
            }
        }
        let total_volume_m3 = components
            .iter()
            .map(|c| c.volume_m3)
            .fold(S::zero(), |a, b| a + b);
        Ok(NativeCutCellPartition {
            native_capacity_m3: capacity
                .iter()
                .map(|c| (c.cell, c.fluid_capacity_m3))
                .collect(),
            components,
            apertures,
            walls,
            total_volume_m3,
            maximum_volume_defect_m3: max_defect,
            arithmetic_length_m: eps,
        })
    }
}

#[derive(Clone, Debug)]
pub struct CutComponentOverlap<S: Scalar> {
    pub previous: usize,
    pub current: usize,
    pub volume_m3: S,
}
pub fn component_overlaps<S: TransientGasScalar>(
    previous: &NativeCutCellPartition<S>,
    current: &NativeCutCellPartition<S>,
) -> CaeResult<Vec<CutComponentOverlap<S>>> {
    let eps = previous
        .arithmetic_length_m
        .max(current.arithmetic_length_m);
    let mut out = Vec::new();
    for (i, a) in previous.components.iter().enumerate() {
        for (j, b) in current.components.iter().enumerate() {
            if a.cell != b.cell {
                continue;
            }
            let mut volume = S::zero();
            for pa in &a.convex_parts {
                for pb in &b.convex_parts {
                    let mut intersection = pa.clone();
                    for face in pb {
                        let plane = Plane { native_levelset:local_geometry::native_levelset_for_polygon(face), subdomain_face:local_geometry::subdomain_face_for_polygon(face), native_face: local_geometry::native_face_for_polygon(face),
                            local_origin: face
                                .local_geometry
                                .as_ref()
                                .map(|g| (g.anchor_m, g.plane_origin_m)),
                            normal: face.outward_normal.map(|x| -x),
                            origin: face.plane_origin_m,
                            offset: -face.plane_offset_m,
                            boundary: face.boundary.clone(),
                        };
                        intersection = clip(&intersection, &plane, eps)?;
                    }
                    let (v, _) = measure(&intersection)?;
                    if v.value() > 0.
                        && (has_retained_geometry(&intersection)
                            || !dimensionless_zero(
                                &intersection
                                    .iter()
                                    .flat_map(|f| &f.vertices_m)
                                    .copied()
                                    .collect::<Vec<_>>(),
                                eps,
                            ))
                    {
                        volume += v;
                    }
                }
            }
            if volume.value() > 0. {
                out.push(CutComponentOverlap {
                    previous: i,
                    current: j,
                    volume_m3: volume,
                });
            }
        }
    }
    Ok(out)
}
#[derive(Clone, Debug)]
pub struct NativeCutCellLayout {
    cells: Vec<usize>,
    slots_per_cell: usize,
    populations: usize,
    state_size: usize,
}
#[derive(Clone, Debug)]
pub struct CutInventoryRemap<S: Scalar> {
    pub inventories_kg: Vec<S>,
    pub active_slots: Vec<bool>,
    pub overlap: Vec<CutComponentOverlap<S>>,
    pub maximum_population_balance_defect_kg: f64,
    pub maximum_unrepresented_donor_volume_m3: f64,
}
impl NativeCutCellLayout {
    pub fn new(
        mut cells: Vec<usize>,
        slots_per_cell: usize,
        populations: usize,
    ) -> CaeResult<Self> {
        cells.sort_unstable();
        if cells.is_empty()
            || slots_per_cell == 0
            || populations == 0
            || cells.windows(2).any(|w| w[0] == w[1])
        {
            return Err(fail("invalid cut-cell state layout"));
        }
        let state_size = cells
            .len()
            .checked_mul(slots_per_cell)
            .and_then(|n| n.checked_mul(populations))
            .ok_or_else(|| fail("cut-cell state size overflow"))?;
        Ok(Self {
            cells,
            slots_per_cell,
            populations,
            state_size,
        })
    }
    pub fn state_size(&self) -> usize {
        self.state_size
    }
    pub fn slot<S: Scalar>(&self, c: &CutCellComponent<S>) -> CaeResult<usize> {
        let cell = self
            .cells
            .binary_search(&c.cell)
            .map_err(|_| fail("component cell is absent from state layout"))?;
        if c.component >= self.slots_per_cell {
            return Err(fail("cut-cell component slots exhausted"));
        }
        Ok(cell * self.slots_per_cell + c.component)
    }
    pub fn remap<S: TransientGasScalar>(
        &self,
        previous: &NativeCutCellPartition<S>,
        current: &NativeCutCellPartition<S>,
        inventory: &[S],
    ) -> CaeResult<CutInventoryRemap<S>> {
        if inventory.len() != self.state_size
            || inventory
                .iter()
                .any(|v| !v.value().is_finite() || v.value() < 0.)
        {
            return Err(fail("invalid cut-cell molecular inventory"));
        }
        let mut old_slots = BTreeSet::new();
        for c in &previous.components {
            if !old_slots.insert(self.slot(c)?) {
                return Err(fail("duplicate old cut-cell slot"));
            }
        }
        for slot in 0..self.state_size / self.populations {
            if !old_slots.contains(&slot)
                && inventory[slot * self.populations..(slot + 1) * self.populations]
                    .iter()
                    .any(|v| v.value() != 0.)
            {
                return Err(fail("inactive cut-cell slot contains gas"));
            }
        }
        let mut active_slots = vec![false; self.state_size / self.populations];
        for c in &current.components {
            let slot = self.slot(c)?;
            if active_slots[slot] {
                return Err(fail("duplicate new cut-cell slot"));
            }
            active_slots[slot] = true;
        }
        let overlap = component_overlaps(previous, current)?;
        let mut inventories = vec![S::zero(); self.state_size];
        let mut unrepresented: f64 = 0.;
        for (i, c) in previous.components.iter().enumerate() {
            let old = self.slot(c)?;
            let total: S = overlap
                .iter()
                .filter(|o| o.previous == i)
                .map(|o| o.volume_m3)
                .fold(S::zero(), |a, b| a + b);
            if total.value() == 0. {
                if inventory[old * self.populations..(old + 1) * self.populations]
                    .iter()
                    .any(|v| v.value() > 0.)
                {
                    return Err(fail(
                        "disappearing gas component retains molecular inventory",
                    ));
                }
                continue;
            }
            let destinations: Vec<_> = overlap.iter().filter(|o| o.previous == i).collect();
            let persistent = destinations.len() == 1
                && overlap
                    .iter()
                    .filter(|o| o.current == destinations[0].current)
                    .count()
                    == 1;
            let missing = (c.volume_m3.value() - total.value()).max(0.);
            unrepresented = unrepresented.max(missing);
            if !persistent && missing > 512. * f64::EPSILON * c.volume_m3.value() {
                return Err(fail(
                    "changing gas connectivity needs resolved transport before remap",
                ));
            }
            for o in overlap.iter().filter(|o| o.previous == i) {
                let new = self.slot(&current.components[o.current])?;
                let weight = o.volume_m3 / total;
                for q in 0..self.populations {
                    inventories[new * self.populations + q] +=
                        inventory[old * self.populations + q] * weight;
                }
            }
        }
        let mut defect: f64 = 0.;
        for q in 0..self.populations {
            let before = (0..self.state_size / self.populations)
                .map(|s| inventory[s * self.populations + q].value())
                .sum::<f64>();
            let after = (0..self.state_size / self.populations)
                .map(|s| inventories[s * self.populations + q].value())
                .sum::<f64>();
            defect = defect.max((after - before).abs());
            if !after.is_finite()
                || (after - before).abs()
                    > 128. * f64::EPSILON * before.abs().max(f64::MIN_POSITIVE)
            {
                return Err(fail("cut-cell remap loses molecular inventory"));
            }
        }
        Ok(CutInventoryRemap {
            inventories_kg: inventories,
            active_slots,
            overlap,
            maximum_population_balance_defect_kg: defect,
            maximum_unrepresented_donor_volume_m3: unrepresented,
        })
    }
}
