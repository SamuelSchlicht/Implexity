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

use crate::native_cut_cell_partition::{
    self as geometry, CutBoundary, CutCellAperture, CutCellComponent, CutCellWall, CutPolygon,
    NativeCutCellPartition, Plane,
};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use std::collections::{BTreeMap, BTreeSet};
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
#[derive(Clone, Debug)]
pub struct CutTetraLocalGeometry<S: Scalar> {
    pub(crate) anchor_m: [f64; 3],
    pub(crate) vertices_m: [[S; 3]; 4],
    pub(crate) chart_sources:Option<[crate::native_geometry_source::NativeChartPointSource<S>;4]>,
    pub(crate) native_supports:Option<[Option<geometry::SubdomainNativeSupport<S>>;4]>,
}
impl<S: Scalar> CutTetraLocalGeometry<S> {
    pub fn anchor(&self) -> [f64; 3] {
        self.anchor_m
    }
    pub fn vertices(&self) -> &[[S; 3]; 4] {
        &self.vertices_m
    }
    pub fn map_scalars<T: Scalar>(&self, map: impl Fn(S) -> T) -> CutTetraLocalGeometry<T> {
        CutTetraLocalGeometry {
            anchor_m: self.anchor_m,
            vertices_m: self.vertices_m.map(|p| p.map(&map)),
            chart_sources:self.chart_sources.as_ref().map(|p|p.clone().map(|p|p.map_scalars(&map))),
            native_supports:self.native_supports.as_ref().map(|p|p.clone().map(|p|p.map(|p|p.map_scalars(&map)))),
        }
    }
}
impl<S: crate::gas_transient_kinetic::TransientGasScalar> CutTetraLocalGeometry<S> {
    pub(crate) fn set_chart_sources(&mut self,sources:[crate::native_geometry_source::NativeChartPointSource<S>;4])->CaeResult<()> {
        for i in 0..4 {let point=sources[i].point_in_frame(self.anchor_m)?;if !(0..3).all(|k|full_zero(point[k]-self.vertices_m[i][k])){return Err(fail("refinement chart source view differs"));}}
        self.chart_sources=Some(sources);self.native_supports=None;Ok(())
    }
    pub(crate) fn set_native_supports(&mut self,supports:[Option<geometry::SubdomainNativeSupport<S>>;4])->CaeResult<()> {
        let sources=self.chart_sources.as_ref().ok_or_else(||fail("refinement chart source is absent"))?;
        for (face,indices) in [[0,1,2],[0,1,3],[0,2,3],[1,2,3]].into_iter().enumerate(){if supports[face].as_ref().is_some_and(|p|!indices.iter().all(|&i|p.owns_chart_point(&sources[i]))){return Err(fail("refinement native support differs"));}}
        self.native_supports=Some(supports);Ok(())
    }
    pub fn new(anchor_m: [f64; 3], vertices_m: [[S; 3]; 4]) -> CaeResult<Self> {
        if anchor_m.iter().any(|v| !v.is_finite())
            || vertices_m
                .iter()
                .flatten()
                .any(|v| !v.finite_first_order() || v.geometry_first_order_directions().is_none())
        {
            return Err(fail("refinement local geometry has incomplete channels"));
        }
        Ok(Self {
            anchor_m,
            vertices_m,
            chart_sources:None,native_supports:None,
        })
    }
}
#[derive(Clone, Debug)]
pub struct CutTetraSubdomain<S: Scalar> {
    pub region: usize,
    pub vertex_ids: Option<[usize; 4]>,
    pub vertices_m: [[S; 3]; 4],
    pub local_geometry: Option<CutTetraLocalGeometry<S>>,
}
impl<S: Scalar> CutTetraSubdomain<S> {
    pub(crate) fn frame_anchor(&self) -> [f64; 3] {
        self.local_geometry
            .as_ref()
            .map(|g| g.anchor_m)
            .unwrap_or_else(|| self.vertices_m[0].map(|v| v.value()))
    }
    pub(crate) fn vertices_in_frame(&self, anchor: [f64; 3]) -> [[S; 3]; 4] {
        if let Some(local) = &self.local_geometry {
            local
                .vertices_m
                .map(|p| std::array::from_fn(|k| p[k] + S::from_f64(local.anchor_m[k] - anchor[k])))
        } else {
            self.vertices_m
                .map(|p| std::array::from_fn(|k| p[k] - S::from_f64(anchor[k])))
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MacroFacetClipSource<S: Scalar> {
    pub(crate) triangle: [[S; 3]; 3],
    pub(crate) local_triangle: Option<geometry::CutLocalGeometry<S>>,
    pub(crate) vertex_ids: [usize; 3],
    pub(crate) regions: (Option<usize>, Option<usize>),
    pub(crate) cell: usize,
    pub(crate) bounds: [[f64; 3]; 2],
    pub(crate) local_bounds: Option<([f64; 3], [[f64; 3]; 2])>,
    pub(crate) patches: Vec<ApertureClipSource<S>>,
}
#[derive(Clone, Debug)]
enum ApertureConstruction<S: Scalar> {
    ConvexClip,
    MacroFacet(MacroFacetClipSource<S>),
}
#[derive(Clone, Debug)]
pub(crate) struct ApertureClipSource<S: Scalar> {
    left: usize,
    right: usize,
    left_part: usize,
    right_part: usize,
    left_face: usize,
    right_face: usize,
    source: Vec<CutPolygon<S>>,
    target: Vec<CutPolygon<S>>,
    aperture: CutCellAperture<S>,
    eps: f64,
    construction: ApertureConstruction<S>,
}
impl<S: Scalar> ApertureClipSource<S> {
    pub(crate) fn owners(&self) -> (usize, usize, usize, usize, usize, usize) {
        (
            self.left,
            self.right,
            self.left_part,
            self.right_part,
            self.left_face,
            self.right_face,
        )
    }
    pub(crate) fn source(&self) -> &[CutPolygon<S>] {
        &self.source
    }
    pub(crate) fn target(&self) -> &[CutPolygon<S>] {
        &self.target
    }
    pub(crate) fn aperture(&self) -> &CutCellAperture<S> {
        &self.aperture
    }
    pub(crate) fn macro_facet(&self) -> Option<&MacroFacetClipSource<S>> {
        match &self.construction {
            ApertureConstruction::MacroFacet(f) => Some(f),
            _ => None,
        }
    }
    pub(crate) fn from_macro_facet(
        patches: Vec<Self>,
        triangle: [[S; 3]; 3],
        local_triangle: Option<geometry::CutLocalGeometry<S>>,
        vertex_ids: [usize; 3],
        regions: (Option<usize>, Option<usize>),
        cell: usize,
        bounds: [[f64; 3]; 2],
        local_bounds: Option<([f64; 3], [[f64; 3]; 2])>,
        aperture: CutCellAperture<S>,
    ) -> CaeResult<Self> {
        let mut receipt = patches
            .first()
            .cloned()
            .ok_or_else(|| fail("macro facet source patches are absent"))?;
        if patches.iter().any(|p| p.macro_facet().is_some()) {
            return Err(fail("macro facet source cannot nest"));
        }
        receipt.aperture = aperture;
        receipt.construction = ApertureConstruction::MacroFacet(MacroFacetClipSource {
            triangle,
            local_triangle,
            vertex_ids,
            regions,
            cell,
            bounds,
            local_bounds,
            patches,
        });
        Ok(receipt)
    }
    pub(crate) fn replay(&self) -> CaeResult<Vec<[S; 3]>>
    where
        S: crate::gas_transient_kinetic::TransientGasScalar,
    {
        Ok(self.replay_local()?.vertices_m)
    }
    pub(crate) fn replay_local(&self) -> CaeResult<CutPolygon<S>>
    where
        S: crate::gas_transient_kinetic::TransientGasScalar,
    {
        if let Some(f) = self.macro_facet() {
            let source = self
                .source
                .get(self.left_face)
                .ok_or_else(|| fail("macro aperture source face is absent"))?;
            let mut polygon = CutPolygon {
                support_only: false,
                vertices_m: f.triangle.to_vec(),
                local_geometry: f.local_triangle.clone(),
                outward_normal: self.aperture.normal,
                plane_origin_m: f.triangle[0],
                plane_offset_m: S::zero(),
                boundary: source.boundary.clone(),
            };
            let (anchor,local_bounds)=f.local_bounds.unwrap_or((f.bounds[0],std::array::from_fn(|side|std::array::from_fn(|k|f.bounds[side][k]-f.bounds[0][k]))));
            if anchor.iter().chain(local_bounds.iter().flatten()).any(|v|!v.is_finite()) {return Err(fail("macro aperture local bounds are nonfinite"));}
            geometry::bind_polygon_frame(&mut polygon, anchor)?;
            for axis in 0..3 {
                for side in 0..2 {
                    let plane = Plane { native_levelset:None, subdomain_face:None, native_face: None,
                        normal: std::array::from_fn(|k| {
                            S::from_f64(if k == axis {
                                if side == 0 {
                                    1.
                                } else {
                                    -1.
                                }
                            } else {
                                0.
                            })
                        }),
                        origin: std::array::from_fn(|k|S::from_f64(local_bounds[side][k]+anchor[k])),
                        local_origin: Some((anchor,local_bounds[side].map(S::from_f64))),
                        offset: S::zero(),
                        boundary: CutBoundary::Cell { axis, side },
                    };
                    polygon = geometry::clip(&[polygon], &plane, self.eps)?
                        .into_iter()
                        .next()
                        .ok_or_else(|| fail("macro aperture clipping lost its owner"))?;
                }
            }
            return Ok(polygon);
        }
        let a = self
            .source
            .get(self.left_face)
            .ok_or_else(|| fail("aperture source face is absent"))?;
        let b = self
            .target
            .get(self.right_face)
            .ok_or_else(|| fail("aperture target face is absent"))?;
        geometry::polygon_part_intersection_local(a, b, &self.target, self.eps)
    }
}
#[derive(Clone, Debug)]
pub struct RefinedCutCellPartition<S: Scalar> {
    pub partition: NativeCutCellPartition<S>,
    pub regions: Vec<Option<usize>>,
    pub(crate) aperture_sources: Vec<Option<ApertureClipSource<S>>>,
    pub maximum_subdomain_volume_defect_m3: f64,
    pub maximum_subdomain_roundoff_bound_m3: f64,
}
pub(crate) fn canonical_face<S: Scalar>(
    tet: &CutTetraSubdomain<S>,
    face: usize,
) -> CaeResult<[[S; 3]; 3]> {
    let mut indices = *[[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]]
        .get(face)
        .ok_or_else(|| fail("invalid refinement face index"))?;
    if let Some(ids) = tet.vertex_ids {
        if (0..4).any(|i| (i + 1..4).any(|j| ids[i] == ids[j])) {
            return Err(fail("duplicate refinement vertex identifier"));
        }
        indices.sort_by_key(|&i| ids[i]);
    } else {
        indices.sort_by(|&i, &j| {
            for k in 0..3 {
                let vertices = tet.vertices_in_frame(tet.frame_anchor());
                let order = vertices[i][k].value().total_cmp(&vertices[j][k].value());
                if !order.is_eq() {
                    return order;
                }
            }
            std::cmp::Ordering::Equal
        });
    }
    Ok(indices.map(|i| tet.vertices_m[i]))
}
pub(crate) fn canonical_face_in_frame<S: Scalar>(
    tet: &CutTetraSubdomain<S>,
    face: usize,
    anchor: [f64; 3],
) -> CaeResult<[[S; 3]; 3]> {
    let mut indices = *[[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]]
        .get(face)
        .ok_or_else(|| fail("invalid refinement face index"))?;
    if let Some(ids) = tet.vertex_ids {
        if (0..4).any(|i| (i + 1..4).any(|j| ids[i] == ids[j])) {
            return Err(fail("duplicate refinement vertex identifier"));
        }
        indices.sort_by_key(|&i| ids[i]);
    } else {
        indices.sort_by(|&i, &j| {
            for k in 0..3 {
                let vertices = tet.vertices_in_frame(tet.frame_anchor());
                let order = vertices[i][k].value().total_cmp(&vertices[j][k].value());
                if !order.is_eq() {
                    return order;
                }
            }
            std::cmp::Ordering::Equal
        });
    }
    Ok(indices.map(|i| tet.vertices_in_frame(anchor)[i]))
}
fn full_zero<S: crate::gas_transient_kinetic::TransientGasScalar>(value: S) -> bool {
    value.finite_first_order()
        && value.value() == 0.
        && value
            .geometry_first_order_directions()
            .is_some_and(|d| d.into_iter().all(|v| v.is_finite() && v == 0.))
}
#[derive(Clone)]
pub(crate) struct RefinementVertex<S: Scalar> {
    anchor_m: [f64; 3],
    local_m: [S; 3],
}
impl<S: Scalar> RefinementVertex<S> {
    fn from_tetra(tet: &CutTetraSubdomain<S>, vertex: usize) -> Self {
        let anchor_m = tet.frame_anchor();
        Self {
            anchor_m,
            local_m: tet.vertices_in_frame(anchor_m)[vertex],
        }
    }
    fn in_frame(&self, anchor: [f64; 3]) -> [S; 3] {
        std::array::from_fn(|k| self.local_m[k] + S::from_f64(self.anchor_m[k] - anchor[k]))
    }
}
fn polygon_anchor<S: Scalar>(face: &CutPolygon<S>) -> [f64; 3] {
    face.local_geometry
        .as_ref()
        .map(|g| g.anchor_m)
        .unwrap_or([0.; 3])
}
fn vertices_in_frame<S: Scalar>(face: &CutPolygon<S>, anchor: [f64; 3]) -> Vec<[S; 3]> {
    geometry::polygon_vertices_in_frame(face, anchor)
}
fn origin_in_frame<S: Scalar>(face: &CutPolygon<S>, anchor: [f64; 3]) -> [S; 3] {
    geometry::polygon_plane_origin_in_frame(face, anchor)
}
fn tetra_authority<S: Scalar>(tet: &CutTetraSubdomain<S>) -> ([f64; 3], [[S; 3]; 4]) {
    tet.local_geometry
        .as_ref()
        .map(|g| (g.anchor_m, g.vertices_m))
        .unwrap_or(([0.; 3], tet.vertices_m))
}
fn part_disjoint<S: crate::gas_transient_kinetic::TransientGasScalar>(
    part: &[CutPolygon<S>],
    tet: &CutTetraSubdomain<S>,
) -> CaeResult<bool> {
    let anchor = part
        .iter()
        .find_map(|f| f.local_geometry.as_ref().map(|g| g.anchor_m))
        .unwrap_or([0.; 3]);
    if part.iter().any(|f| f.local_geometry.is_some())
        && part.iter().any(|f| f.local_geometry.is_none())
    {
        return Err(fail("refinement part has mixed geometry authorities"));
    }
    if part.iter().any(|f| {
        f.local_geometry
            .as_ref()
            .is_some_and(|g| g.anchor_m.map(f64::to_bits) != anchor.map(f64::to_bits))
    }) {
        return Err(fail("refinement part has different local owners"));
    }
    let vertices = part
        .iter()
        .filter(|f| !f.support_only)
        .flat_map(|f| vertices_in_frame(f, anchor))
        .collect::<Vec<_>>();
    let (tet_anchor, tet_vertices) = tetra_authority(tet);
    framed_disjoint(&vertices, anchor, &tet_vertices, tet_anchor)
}
pub(crate) fn zero_area_numerical_aperture<S: crate::gas_transient_kinetic::TransientGasScalar>(
    a: &CutPolygon<S>,
    b: &CutPolygon<S>,
    source: &[CutPolygon<S>],
    target: &[CutPolygon<S>],
    faces: &BTreeMap<usize, Option<[[usize; 3]; 4]>>,
    vertices: &BTreeMap<usize, RefinementVertex<S>>,
) -> CaeResult<bool> {
    let face_ids = |boundary: &CutBoundary| -> CaeResult<Option<[usize; 3]>> {
        let CutBoundary::Subdomain { region, face } = boundary else {
            return Ok(None);
        };
        let Some(keys) = faces
            .get(region)
            .ok_or_else(|| fail("numerical aperture plane parent is missing"))?
        else {
            return Ok(None);
        };
        let mut key = *keys
            .get(*face)
            .ok_or_else(|| fail("invalid numerical aperture face"))?;
        key.sort();
        Ok(Some(key))
    };
    let Some(key) = face_ids(&a.boundary)? else {
        return Ok(false);
    };
    if face_ids(&b.boundary)? != Some(key) {
        return Ok(false);
    }
    for id in key {
        let producer = vertices
            .get(&id)
            .ok_or_else(|| fail("numerical aperture face vertex is missing"))?;
        for facet in [a, b] {
            let anchor = polygon_anchor(facet);
            let point = producer.in_frame(anchor);
            let origin = origin_in_frame(facet, anchor);
            let d = dot(facet.outward_normal, sub(point, origin)) + facet.plane_offset_m;
            let scale = (0..3)
                .map(|k| {
                    facet.outward_normal[k].value().abs()
                        * (point[k].value().abs() + origin[k].value().abs())
                })
                .sum::<f64>()
                + facet.plane_offset_m.value().abs();
            if !d.value().is_finite() || !scale.is_finite() {
                return Err(fail("numerical aperture face incidence overflow"));
            }
            if d.value().abs()
                > if facet.local_geometry.is_some() {
                    0.
                } else {
                    128. * f64::EPSILON * scale
                }
                || (facet.local_geometry.is_some() && !full_zero(d))
            {
                return Ok(false);
            }
        }
    }
    for (facet, constraints) in [(a, source), (b, target)] {
        for plane in constraints {
            let anchor = polygon_anchor(facet);
            let origin = origin_in_frame(plane, anchor);
            for point in vertices_in_frame(facet, anchor) {
                let d = dot(plane.outward_normal, sub(point, origin)) + plane.plane_offset_m;
                let scale = (0..3)
                    .map(|k| {
                        plane.outward_normal[k].value().abs()
                            * (point[k].value().abs() + origin[k].value().abs())
                    })
                    .sum::<f64>()
                    + plane.plane_offset_m.value().abs();
                if !d.value().is_finite() || !scale.is_finite() {
                    return Err(fail("numerical aperture containment overflow"));
                }
                if d.value()
                    > if facet.local_geometry.is_some() || plane.local_geometry.is_some() {
                        0.
                    } else {
                        128. * f64::EPSILON * scale
                    }
                {
                    return Ok(false);
                }
            }
        }
    }
    let mut sides: BTreeMap<[usize; 2], u8> = BTreeMap::new();
    for plane in source.iter().chain(target) {
        let Some(ids) = face_ids(&plane.boundary)? else {
            continue;
        };
        let shared = ids
            .into_iter()
            .filter(|i| key.contains(i))
            .collect::<Vec<_>>();
        if shared.len() != 2 {
            continue;
        }
        let edge = [shared[0], shared[1]];
        let anchor = polygon_anchor(plane);
        let origin = origin_in_frame(plane, anchor);
        let producers = edge.map(|id| {
            vertices
                .get(&id)
                .ok_or_else(|| fail("numerical aperture edge vertex is missing"))
        });
        let local_points = [
            producers[0]
                .as_ref()
                .map_err(|_| fail("numerical aperture edge vertex is missing"))?
                .in_frame(anchor),
            producers[1]
                .as_ref()
                .map_err(|_| fail("numerical aperture edge vertex is missing"))?
                .in_frame(anchor),
        ];
        let incident = local_points.iter().all(|point| {
            let d = dot(plane.outward_normal, sub(*point, origin)) + plane.plane_offset_m;
            let scale = (0..3)
                .map(|k| {
                    plane.outward_normal[k].value().abs()
                        * (point[k].value().abs() + origin[k].value().abs())
                })
                .sum::<f64>()
                + plane.plane_offset_m.value().abs();
            d.value().is_finite()
                && scale.is_finite()
                && d.value().abs()
                    <= if plane.local_geometry.is_some() {
                        0.
                    } else {
                        128. * f64::EPSILON * scale
                    }
                && (plane.local_geometry.is_none() || full_zero(d))
        });
        if !incident {
            continue;
        }
        let projected = cross(sub(local_points[1], local_points[0]), a.outward_normal);
        let orientation = dot(projected, plane.outward_normal).value();
        if !orientation.is_finite() {
            return Err(fail("numerical aperture edge orientation overflow"));
        }
        let sign_roundoff = 32.
            * f64::EPSILON
            * dot(projected, projected).value().sqrt()
            * dot(plane.outward_normal, plane.outward_normal)
                .value()
                .sqrt();
        if !sign_roundoff.is_finite() {
            return Err(fail("numerical aperture edge sign overflow"));
        }
        if orientation.abs()
            <= if plane.local_geometry.is_some() || a.local_geometry.is_some() {
                0.
            } else {
                sign_roundoff
            }
        {
            continue;
        }
        let side = if orientation > 0. {
            1
        } else if orientation < 0. {
            2
        } else {
            0
        };
        let entry = sides.entry(edge).or_default();
        *entry |= side;
        if *entry == 3 {
            return Ok(true);
        }
    }
    Ok(false)
}
pub(crate) fn tetra_planes<S: crate::gas_transient_kinetic::TransientGasScalar>(tet: &CutTetraSubdomain<S>) -> CaeResult<(Vec<Plane<S>>, S)> {
    let anchor = tet.frame_anchor();
    let p = tet.vertices_in_frame(anchor);
    let determinant = dot(sub(p[1], p[0]), cross(sub(p[2], p[0]), sub(p[3], p[0])));
    if !determinant.value().is_finite() || determinant.value() <= 0. {
        return Err(fail("refinement tetrahedron must have positive volume"));
    }
    let mut planes = vec![];
    for (face, f) in [[0, 1, 2, 3], [0, 1, 3, 2], [0, 2, 3, 1], [1, 2, 3, 0]]
        .into_iter()
        .enumerate()
    {
        let canonical = canonical_face_in_frame(tet, face, anchor)?;
        let n = cross(
            sub(canonical[1], canonical[0]),
            sub(canonical[2], canonical[0]),
        );
        let length = dot(n, n).sqrt();
        let mut n = n.map(|v| v / length);
        if dot(n, sub(p[f[3]], canonical[0])).value() < 0. {
            n = n.map(|v| -v);
        }
        let vertex_ids=tet.vertex_ids.map(|ids|{let mut vertices=f[..3].iter().map(|&i|ids[i]).collect::<Vec<_>>();vertices.sort_unstable();[vertices[0],vertices[1],vertices[2]]});
        let mut source_indices=[f[0],f[1],f[2]];
        if let Some(ids)=tet.vertex_ids {source_indices.sort_by_key(|&i|ids[i]);}else{source_indices.sort_by(|&i,&j|{for k in 0..3{let order=p[i][k].value().total_cmp(&p[j][k].value());if !order.is_eq(){return order;}}std::cmp::Ordering::Equal});}
        let support=tet.local_geometry.as_ref().and_then(|g|g.native_supports.as_ref()).and_then(|p|p[face].clone());
        let (normal,origin,offset)=if let Some(support)=&support {let plane=support.plane()?;let normal=if dot(plane.normal,sub(p[f[3]],canonical[0])).value()<0.{plane.normal.map(|v|-v)}else{plane.normal};let sign=if (0..3).all(|k|full_zero(normal[k]-plane.normal[k])){1.}else{-1.};(normal,geometry::plane_origin_in_frame(&plane,anchor),plane.offset*sign)}else{(n,canonical[0],S::zero())};
        let mut receipt=geometry::SubdomainPlaneFace::new(tet.region,face,vertex_ids,canonical,normal,offset);receipt.set_anchor(anchor);
        if let Some(sources)=tet.local_geometry.as_ref().and_then(|g|g.chart_sources.as_ref()){receipt.set_chart_sources(source_indices.map(|i|sources[i].clone()));}
        if let Some(support)=support{receipt.set_native_support(support);}
        planes.push(Plane { native_levelset:None, subdomain_face:Some(receipt), native_face: None,
            normal,
            origin: std::array::from_fn(|k| origin[k] + S::from_f64(anchor[k])),
            local_origin: Some((anchor, origin)),
            offset,
            boundary: CutBoundary::Subdomain {
                region: tet.region,
                face,
            },
        });
    }
    Ok((planes, determinant / 6.))
}
fn tetra_poly<S: crate::gas_transient_kinetic::TransientGasScalar>(
    tet: &CutTetraSubdomain<S>,
    planes: &[Plane<S>],
) -> CaeResult<Vec<CutPolygon<S>>> {
    let anchor = tet.frame_anchor();
    [[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]]
        .into_iter()
        .enumerate()
        .map(|(i, f)| {
            let mut polygon = CutPolygon {
                support_only: false,
                local_geometry: None,
                vertices_m: f.map(|j| tet.vertices_m[j]).to_vec(),
                outward_normal: planes[i].normal.map(|v| -v),
                plane_origin_m: planes[i].origin,
                plane_offset_m: -planes[i].offset,
                boundary: planes[i].boundary.clone(),
            };
            geometry::bind_polygon_frame(&mut polygon, anchor)?;
            if let Some(local) = &mut polygon.local_geometry {
                local.vertices_m = f.map(|j| tet.vertices_in_frame(anchor)[j]).to_vec();
                if let Some(sources)=tet.local_geometry.as_ref().and_then(|g|g.chart_sources.as_ref()){local.point_constructions=f.map(|j|geometry::PointConstruction::from_chart_source(&sources[j],tet.vertices_in_frame(anchor)[j],anchor)).to_vec();}
                local.plane_origin_m = planes[i]
                    .local_origin
                    .map(|(other, p)| {
                        std::array::from_fn(|k| p[k] + S::from_f64(other[k] - anchor[k]))
                    })
                    .unwrap_or_else(|| {
                        std::array::from_fn(|k| planes[i].origin[k] - S::from_f64(anchor[k]))
                    });
            }
            for (vertex,index) in f.into_iter().enumerate(){for (face,indices) in [[0,1,2],[0,1,3],[0,2,3],[1,2,3]].into_iter().enumerate(){if indices.contains(&index){
                if !planes[face].subdomain_face.as_ref().is_some_and(|r|r.valid(&planes[face])){return Err(fail("refinement original face receipt is invalid"));}
                geometry::seed_vertex_plane(&mut polygon,vertex,&planes[face])?;
            }}}
            Ok(polygon)
        })
        .collect()
}
fn framed_disjoint<S: crate::gas_transient_kinetic::TransientGasScalar>(
    a: &[[S; 3]],
    a_anchor: [f64; 3],
    b: &[[S; 3]],
    b_anchor: [f64; 3],
) -> CaeResult<bool> {
    if a.is_empty() || b.is_empty() {
        return Ok(true);
    }
    for axis in 0..3 {
        let low_a = a
            .iter()
            .min_by(|p, q| p[axis].value().total_cmp(&q[axis].value()))
            .unwrap();
        let high_a = a
            .iter()
            .max_by(|p, q| p[axis].value().total_cmp(&q[axis].value()))
            .unwrap();
        let low_b = b
            .iter()
            .min_by(|p, q| p[axis].value().total_cmp(&q[axis].value()))
            .unwrap();
        let high_b = b
            .iter()
            .max_by(|p, q| p[axis].value().total_cmp(&q[axis].value()))
            .unwrap();
        let normal = std::array::from_fn(|k| if k == axis { 1. } else { 0. });
        for (left, left_anchor, right, right_anchor) in [
            (high_a, a_anchor, low_b, b_anchor),
            (high_b, b_anchor, low_a, a_anchor),
        ] {
            let sign = crate::native_exact_affine_predicate::exact_framed_plane_sign(
                normal,
                left.map(|v| v.value()),
                left_anchor,
                right.map(|v| v.value()),
                right_anchor,
                0.,
            )
            .map_err(|e| fail(&e))?;
            if sign < 0
                || (sign == 0
                    && a.iter()
                        .chain(b)
                        .all(|p| full_zero(p[axis] - S::from_f64(p[axis].value()))))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
fn intersection<S: crate::gas_transient_kinetic::TransientGasScalar>(
    poly: &[CutPolygon<S>],
    planes: &[Plane<S>],
    eps: f64,
) -> CaeResult<Vec<CutPolygon<S>>> {
    let mut out = poly.to_vec();
    for p in planes {
        out = geometry::clip(&out, p, eps)?;
    }
    Ok(out)
}
fn has_volume<S: crate::gas_transient_kinetic::TransientGasScalar>(
    p: &[CutPolygon<S>],
    eps: f64,
) -> CaeResult<bool> {
    let local = p.iter().find_map(|f| f.local_geometry.as_ref());
    let anchor = local.map(|g| g.anchor_m).unwrap_or([0.; 3]);
    Ok(geometry::measure(p)?.0.value() > 0.
        && !geometry::dimensionless_zero(
            &p.iter()
                .flat_map(|f| vertices_in_frame(f, anchor))
                .collect::<Vec<_>>(),
            if local.is_some() { 0. } else { eps },
        ))
}
fn root(p: &mut [usize], i: usize) -> usize {
    if p[i] != i {
        p[i] = root(p, p[i]);
    }
    p[i]
}
pub fn refine<S: crate::gas_transient_kinetic::TransientGasScalar>(
    base: &NativeCutCellPartition<S>,
    tets: &[CutTetraSubdomain<S>],
) -> CaeResult<RefinedCutCellPartition<S>> {
    let eps = base.arithmetic_length_m;
    if !eps.is_finite() || eps <= 0. {
        return Err(fail("invalid refinement arithmetic tolerance"));
    }
    let mut native_vertices: BTreeMap<usize, RefinementVertex<S>> = BTreeMap::new();
    let mut numerical_faces = BTreeMap::new();
    for tet in tets {
        if let Some(local) = &tet.local_geometry {
            if local.anchor_m.iter().any(|v| !v.is_finite())
                || local.vertices_m.iter().flatten().any(|v| {
                    !v.finite_first_order() || v.geometry_first_order_directions().is_none()
                })
            {
                return Err(fail("refinement local producer has incomplete channels"));
            }
            for i in 0..4 {
                for k in 0..3 {
                    let replay = local.vertices_m[i][k] + S::from_f64(local.anchor_m[k]);
                    if replay.value().to_bits() != tet.vertices_m[i][k].value().to_bits()
                        || !full_zero(replay - tet.vertices_m[i][k])
                    {
                        return Err(fail(
                            "refinement local producer differs from its world view",
                        ));
                    }
                }
            }
        }
        let keys = tet
            .vertex_ids
            .map(|ids| [[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]].map(|f| f.map(|i| ids[i])));
        if numerical_faces.insert(tet.region, keys).is_some() {
            return Err(fail("duplicate numerical gas region identifier"));
        }
        if let Some(ids) = tet.vertex_ids {
            for (i, id) in ids.into_iter().enumerate() {
                if let Some(previous) = native_vertices.get(&id) {
                    let current = RefinementVertex::from_tetra(tet, i);
                    let mut differs = false;
                    for k in 0..3 {
                        let normal = std::array::from_fn(|axis| if axis == k { 1. } else { 0. });
                        let sign = crate::native_exact_affine_predicate::exact_framed_plane_sign(
                            normal,
                            previous.local_m.map(|v| v.value()),
                            previous.anchor_m,
                            current.local_m.map(|v| v.value()),
                            current.anchor_m,
                            0.,
                        )
                        .map_err(|e| fail(&e))?;
                        differs |= sign != 0
                            || !full_zero(
                                previous.local_m[k] - current.in_frame(previous.anchor_m)[k],
                            );
                    }
                    if differs {
                        return Err(fail("shared refinement vertex has inconsistent geometry"));
                    }
                } else {
                    native_vertices.insert(id, RefinementVertex::from_tetra(tet, i));
                }
            }
        }
    }
    let allplanes = tets
        .iter()
        .map(tetra_planes)
        .collect::<CaeResult<Vec<_>>>()?;
    let mut maxdefect: f64 = 0.;
    let mut maxbound: f64 = 0.;
    for (i, (planes, volume)) in allplanes.iter().enumerate() {
        let mut covered = S::zero();
        for c in &base.components {
            for p in &c.convex_parts {
                if part_disjoint(p, &tets[i])? {
                    continue;
                }
                let clipped = intersection(p, planes, eps)?;
                if has_volume(&clipped, eps)? {
                    covered += geometry::measure(&clipped)?.0;
                }
            }
        }
        let defect = (covered.value() - volume.value()).abs();
        maxdefect = maxdefect.max(defect);
        let surface_area: f64 = tetra_poly(&tets[i], planes)?
            .iter()
            .map(|f| geometry::local_polygon_area(f).value())
            .sum();
        let bound = 2048. * f64::EPSILON * volume.value() + 8. * eps * surface_area;
        maxbound = maxbound.max(bound);
        if defect > bound {
            return Err(fail(&format!(
                "refinement tetrahedron {i} is not wholly owned by sharp void volume {:e} covered {:e} defect {:e}",
                volume.value(),
                covered.value(),
                defect
            )));
        }
        for j in 0..i {
            let (first_anchor, first_vertices) = tetra_authority(&tets[i]);
            let (second_anchor, second_vertices) = tetra_authority(&tets[j]);
            if framed_disjoint(
                &first_vertices,
                first_anchor,
                &second_vertices,
                second_anchor,
            )? {
                continue;
            }
            let p = tetra_poly(&tets[i], planes)?;
            let clipped = intersection(&p, &allplanes[j].0, eps)?;
            if has_volume(&clipped, eps)?
                && geometry::measure(&clipped)?.0.value()
                    > 2048. * f64::EPSILON * volume.value().min(allplanes[j].1.value())
            {
                return Err(fail("refinement tetrahedra overlap"));
            }
        }
    }
    let mut pieces: BTreeMap<(usize, Option<usize>), Vec<Vec<CutPolygon<S>>>> = BTreeMap::new();
    for c in &base.components {
        for original in &c.convex_parts {
            let mut residual = vec![original.clone()];
            for (i, (planes, _)) in allplanes.iter().enumerate() {
                let mut next = vec![];
                for part in residual {
                    if part_disjoint(&part, &tets[i])? {
                        next.push(part);
                        continue;
                    }
                    let assigned = intersection(&part, planes, eps)?;
                    if has_volume(&assigned, eps)? {
                        pieces
                            .entry((c.cell, Some(tets[i].region)))
                            .or_default()
                            .push(assigned);
                    }
                    next.extend(geometry::subtract(part, planes, eps, 0.)?);
                }
                residual = next;
            }
            pieces.entry((c.cell, None)).or_default().extend(residual);
        }
    }
    let mut entries = vec![];
    for ((cell, region), parts) in pieces {
        let mut parent: Vec<_> = (0..parts.len()).collect();
        for i in 0..parts.len() {
            for j in 0..i {
                let mut adjacent = false;
                'faces: for a in parts[i].iter().filter(|f| !f.support_only) {
                    for b in parts[j].iter().filter(|f| !f.support_only) {
                        if dot(a.outward_normal, b.outward_normal).value() > -0.5 {
                            continue;
                        }
                        if geometry::local_positive_polygon(&geometry::polygon_intersection_local(
                            a, b, eps,
                        )?) {
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
        for group in groups.into_values() {
            let mut volume = S::zero();
            let mut moment = [S::zero(); 3];
            for p in &group {
                let (v, m) = geometry::measure(p)?;
                volume += v;
                for k in 0..3 {
                    moment[k] += m[k];
                }
            }
            if volume.value() <= 0. {
                return Err(fail("refinement produced nonpositive component"));
            }
            entries.push((cell, region, moment.map(|v| v / volume), volume, group));
        }
    }
    entries.sort_by(|a, b| {
        a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then_with(|| {
            (0..3)
                .map(|k| a.2[k].value().total_cmp(&b.2[k].value()))
                .find(|o| !o.is_eq())
                .unwrap_or_else(|| a.3.value().total_cmp(&b.3.value()))
        })
    });
    let mut local: BTreeMap<usize, usize> = BTreeMap::new();
    let mut components = vec![];
    let mut regions = vec![];
    for (cell, region, centroid, volume, parts) in entries {
        let id = local.entry(cell).or_default();
        let local_centroid_m=if let Some(frame)=parts.iter().flatten().find_map(|face|face.local_geometry.as_ref().map(|g|g.anchor())) {
            let mut local_volume=S::zero();
            let mut local_moment=[S::zero();3];
            for part in &parts {
                let (volume,moment)=geometry::measure_in_frame(part,frame)?;
                local_volume+=volume;
                for k in 0..3 {local_moment[k]+=moment[k];}
            }
            if !local_volume.finite_first_order() || local_volume.value()<=0. {return Err(fail("refined local centroid volume must be positive"));}
            let centre=local_moment.map(|moment|moment/local_volume);
            if centre.iter().any(|v|!v.finite_first_order() || !v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|v|v.is_finite()))) {return Err(fail("refined local centroid channels are invalid"));}
            Some((frame,centre))
        }else{None};
        components.push(CutCellComponent {
            cell,
            component: *id,
            volume_m3: volume,
            centroid_m: centroid,
            local_centroid_m,
            central_second_integral_m5: geometry::centroid_second_integral(&parts, centroid)?,
            convex_parts: parts,
        });
        *id += 1;
        regions.push(region);
    }
    let neighbours: BTreeSet<_> = base
        .apertures
        .iter()
        .filter_map(|a| {
            a.right.map(|j| {
                let mut ids = [base.components[a.left].cell, base.components[j].cell];
                ids.sort();
                ids
            })
        })
        .collect();
    let mut components_by_cell: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (index, component) in components.iter().enumerate() {
        components_by_cell
            .entry(component.cell)
            .or_default()
            .push(index);
    }
    let mut adjacent_cells: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for &[a, b] in &neighbours {
        adjacent_cells.entry(a).or_default().insert(b);
        adjacent_cells.entry(b).or_default().insert(a);
    }
    let mut external_by_cell: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (index, aperture) in base.apertures.iter().enumerate() {
        if aperture.right.is_none() {
            let cell = base
                .components
                .get(aperture.left)
                .ok_or_else(|| fail("external aperture owner is missing"))?
                .cell;
            external_by_cell.entry(cell).or_default().push(index);
        }
    }
    let mut apertures = vec![];
    let mut aperture_sources = vec![];
    let mut walls = vec![];
    for i in 0..components.len() {
        let cell = components[i].cell;
        let mut candidates = components_by_cell.get(&cell).cloned().unwrap_or_default();
        if let Some(cells) = adjacent_cells.get(&cell) {
            for cell in cells {
                if let Some(indices) = components_by_cell.get(cell) {
                    candidates.extend_from_slice(indices);
                }
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        for j in candidates.into_iter().filter(|&j| j < i) {
            let same = components[i].cell == components[j].cell;
            for a in components[i]
                .convex_parts
                .iter()
                .flatten()
                .filter(|f| !f.support_only)
            {
                for b in components[j]
                    .convex_parts
                    .iter()
                    .flatten()
                    .filter(|f| !f.support_only)
                {
                    if dot(a.outward_normal, b.outward_normal).value() > -0.5 {
                        continue;
                    }
                    if !same
                        && !matches!((&a.boundary,&b.boundary),(CutBoundary::Cell{axis:aa,side:sa},CutBoundary::Cell{axis:ab,side:sb})if aa==ab&&sa!=sb)
                    {
                        continue;
                    }
                    let part = components[j]
                        .convex_parts
                        .iter()
                        .find(|part| part.iter().any(|face| std::ptr::eq(face, b)))
                        .ok_or_else(|| fail("aperture convex owner is missing"))?;
                    let source = components[i]
                        .convex_parts
                        .iter()
                        .find(|p| p.iter().any(|f| std::ptr::eq(f, a)))
                        .ok_or_else(|| fail("aperture source convex owner is missing"))?;
                    if zero_area_numerical_aperture(
                        a,
                        b,
                        source,
                        part,
                        &numerical_faces,
                        &native_vertices,
                    )? {
                        continue;
                    }
                    let polygon = geometry::polygon_part_intersection_local(a, b, part, eps)?;
                    if geometry::local_positive_polygon(&polygon) {
                        let axis = if same {
                            None
                        } else {
                            match a.boundary {
                                CutBoundary::Cell { axis, .. } => Some(axis),
                                _ => None,
                            }
                        };
                        apertures.push(CutCellAperture {
                            left: i,
                            right: Some(j),
                            outside_cell: None,
                            axis,
                            normal: a.outward_normal,
                            area_m2: geometry::local_polygon_area(&polygon),
                            local_polygons: polygon.local_geometry.clone().map(|g| vec![g]),
                            polygons: vec![polygon.vertices_m],
                            polygon_parts: vec![(
                                components[i]
                                    .convex_parts
                                    .iter()
                                    .position(|p| std::ptr::eq(p, source))
                                    .unwrap(),
                                Some(
                                    components[j]
                                        .convex_parts
                                        .iter()
                                        .position(|p| std::ptr::eq(p, part))
                                        .unwrap(),
                                ),
                            )],
                        });
                        aperture_sources.push(Some(ApertureClipSource {
                            left: i,
                            right: j,
                            left_part: components[i]
                                .convex_parts
                                .iter()
                                .position(|p| std::ptr::eq(p, source))
                                .unwrap(),
                            right_part: components[j]
                                .convex_parts
                                .iter()
                                .position(|p| std::ptr::eq(p, part))
                                .unwrap(),
                            left_face: source.iter().position(|f| std::ptr::eq(f, a)).unwrap(),
                            right_face: part.iter().position(|f| std::ptr::eq(f, b)).unwrap(),
                            source: source.clone(),
                            target: part.clone(),
                            aperture: apertures.last().unwrap().clone(),
                            eps,
                            construction: ApertureConstruction::ConvexClip,
                        }));
                    }
                }
            }
        }
        for face in components[i].convex_parts.iter().flatten() {
            if face.support_only {
                continue;
            }
            for external in external_by_cell
                .get(&components[i].cell)
                .into_iter()
                .flatten()
                .map(|&index| &base.apertures[index])
            {
                if external
                    .local_polygons
                    .as_ref()
                    .is_some_and(|p| p.len() != external.polygons.len())
                {
                    return Err(fail("external aperture local polygon ownership differs"));
                }
                for (polygon_index, p) in external.polygons.iter().enumerate() {
                    let mut reference = CutPolygon {
                        support_only: false,
                        local_geometry: external
                            .local_polygons
                            .as_ref()
                            .and_then(|p| p.get(polygon_index))
                            .cloned(),
                        vertices_m: p.clone(),
                        outward_normal: external.normal,
                        plane_origin_m: p[0],
                        plane_offset_m: S::zero(),
                        boundary: CutBoundary::Cell {
                            axis: external.axis.unwrap(),
                            side: 0,
                        },
                    };
                    if let Some(local) = &mut reference.local_geometry {
                        local.plane_origin_m = *local
                            .vertices_m
                            .first()
                            .ok_or_else(|| fail("external aperture local vertices are empty"))?;
                    }
                    let retained_owner=reference.local_geometry.as_ref().and_then(|local|local.plane_incidence.as_ref()).map(|plane|plane.boundary().clone());
                    if let Some(boundary)=retained_owner {
                        match boundary {
                            CutBoundary::Cell{axis,side} if Some(axis)==external.axis && side<2=>reference.boundary=boundary,
                            _=>return Err(fail("external aperture original plane owner differs")),
                        }
                    }
                    geometry::bind_polygon_frame(&mut reference, polygon_anchor(face))?;
                    if reference.local_geometry.as_ref().is_some_and(|local|local.plane_incidence.is_some())
                        && !geometry::authentic_edge_source(&reference,polygon_anchor(face))? {return Err(fail("external aperture original halfspace differs"));}
                    if dot(face.outward_normal, external.normal).value() < 0.5 {
                        continue;
                    }
                    let polygon = geometry::polygon_intersection_local(face, &reference, eps)?;
                    if geometry::local_positive_polygon(&polygon) {
                        apertures.push(CutCellAperture {
                            left: i,
                            right: None,
                            outside_cell: external.outside_cell,
                            axis: external.axis,
                            normal: face.outward_normal,
                            area_m2: geometry::local_polygon_area(&polygon),
                            local_polygons: polygon.local_geometry.clone().map(|g| vec![g]),
                            polygons: vec![polygon.vertices_m],
                            polygon_parts: vec![(
                                components[i]
                                    .convex_parts
                                    .iter()
                                    .position(|p| p.iter().any(|f| std::ptr::eq(f, face)))
                                    .unwrap(),
                                None,
                            )],
                        });
                        aperture_sources.push(None);
                    }
                }
            }
            for wall in base
                .walls
                .iter()
                .filter(|w| base.components[w.component].cell == components[i].cell)
            {
                if dot(face.outward_normal, wall.normal).value() < 0.5 {
                    continue;
                }
                let mut reference = CutPolygon {
                    support_only: false,
                    local_geometry: wall.local_geometry.clone(),
                    vertices_m: wall.vertices_m.clone(),
                    outward_normal: wall.normal,
                    plane_origin_m: wall.vertices_m[0],
                    plane_offset_m: S::zero(),
                    boundary: CutBoundary::Native {
                        body: wall.body,
                        element: wall.element,
                        face: None,
                        physical: true,
                    },
                };
                if let Some(local) = &mut reference.local_geometry {
                    local.plane_origin_m = *local
                        .vertices_m
                        .first()
                        .ok_or_else(|| fail("native wall local vertices are empty"))?;
                }
                geometry::bind_polygon_frame(&mut reference, polygon_anchor(face))?;
                let polygon = geometry::polygon_intersection_local(face, &reference, eps)?;
                if !geometry::local_positive_polygon(&polygon) {
                    continue;
                }
                let anchor = polygon_anchor(&polygon);
                let wall_vertices = vertices_in_frame(&reference, anchor);
                let polygon_vertices = vertices_in_frame(&polygon, anchor);
                let p = wall_vertices[0];
                if wall_vertices.len() != wall.native_barycentric.len() {
                    return Err(fail("native wall local interpolation ownership differs"));
                }
                let mut best = (0., 0, 0);
                for a in 1..wall_vertices.len() {
                    for b in a + 1..wall_vertices.len() {
                        let u = sub(wall_vertices[a], p);
                        let v = sub(wall_vertices[b], p);
                        let n = cross(u, v);
                        let score = dot(n, n).value();
                        if score > best.0 {
                            best = (score, a, b);
                        }
                    }
                }
                if best.0 == 0. {
                    return Err(fail("degenerate native wall interpolation"));
                }
                let u = sub(wall_vertices[best.1], p);
                let v = sub(wall_vertices[best.2], p);
                let n = cross(u, v);
                let denominator = dot(n, n);
                let bary = polygon_vertices
                    .iter()
                    .map(|&x| {
                        let delta = sub(x, p);
                        let alpha = dot(cross(delta, v), n) / denominator;
                        let beta = dot(cross(u, delta), n) / denominator;
                        std::array::from_fn(|k| {
                            wall.native_barycentric[0][k]
                                + alpha
                                    * (wall.native_barycentric[best.1][k]
                                        - wall.native_barycentric[0][k])
                                + beta
                                    * (wall.native_barycentric[best.2][k]
                                        - wall.native_barycentric[0][k])
                        })
                    })
                    .collect();
                walls.push(CutCellWall {
                    component: i,
                    convex_part: components[i]
                        .convex_parts
                        .iter()
                        .position(|part| part.iter().any(|f| std::ptr::eq(f, face)))
                        .ok_or_else(|| fail("native wall convex owner missing"))?,
                    body: wall.body,
                    element: wall.element,
                    area_m2: geometry::local_polygon_area(&polygon),
                    normal: wall.normal,
                    local_geometry: polygon.local_geometry.clone(),
                    vertices_m: polygon.vertices_m,
                    native_nodes: wall.native_nodes,
                    native_barycentric: bary,
                });
            }
        }
    }
    let total_volume_m3: S = components
        .iter()
        .map(|c| c.volume_m3)
        .fold(S::zero(), |a, b| a + b);
    let defect = (total_volume_m3.value() - base.total_volume_m3.value()).abs();
    if defect > 4096. * f64::EPSILON * base.total_volume_m3.value() {
        return Err(fail("refinement loses sharp void volume"));
    }
    Ok(RefinedCutCellPartition {
        partition: NativeCutCellPartition {
            native_capacity_m3: base.native_capacity_m3.clone(),
            components,
            apertures,
            walls,
            total_volume_m3,
            maximum_volume_defect_m3: base.maximum_volume_defect_m3.max(defect),
            arithmetic_length_m: eps,
        },
        regions,
        aperture_sources,
        maximum_subdomain_volume_defect_m3: maxdefect,
        maximum_subdomain_roundoff_bound_m3: maxbound,
    })
}
