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
    (0..3).map(|k| a[k] * b[k]).fold(S::zero(), |a,b|a+b)
}
fn cross<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
#[derive(Clone, Debug)]
pub struct CutTetraSubdomain<S: Scalar> {
    pub region: usize,
    pub vertex_ids: Option<[usize; 4]>,
    pub vertices_m: [[S; 3]; 4],
}
#[derive(Clone, Debug)]
pub(crate) struct MacroFacetClipSource<S:Scalar> {
    pub(crate) triangle: [[S;3];3],
    pub(crate) vertex_ids: [usize;3],
    pub(crate) regions: (Option<usize>,Option<usize>),
    pub(crate) cell: usize,
    pub(crate) bounds: [[f64;3];2],
    pub(crate) patches: Vec<ApertureClipSource<S>>,
}
#[derive(Clone, Debug)]
enum ApertureConstruction<S:Scalar> { ConvexClip, MacroFacet(MacroFacetClipSource<S>) }
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
        (self.left,self.right,self.left_part,self.right_part,self.left_face,self.right_face)
    }
    pub(crate) fn source(&self) -> &[CutPolygon<S>] { &self.source }
    pub(crate) fn target(&self) -> &[CutPolygon<S>] { &self.target }
    pub(crate) fn aperture(&self) -> &CutCellAperture<S> { &self.aperture }
    pub(crate) fn macro_facet(&self)->Option<&MacroFacetClipSource<S>> { match &self.construction { ApertureConstruction::MacroFacet(f)=>Some(f),_=>None } }
    pub(crate) fn from_macro_facet(patches:Vec<Self>,triangle:[[S;3];3],vertex_ids:[usize;3],regions:(Option<usize>,Option<usize>),cell:usize,bounds:[[f64;3];2],aperture:CutCellAperture<S>)->CaeResult<Self> {
        let mut receipt=patches.first().cloned().ok_or_else(||fail("macro facet source patches are absent"))?;
        if patches.iter().any(|p|p.macro_facet().is_some()){return Err(fail("macro facet source cannot nest"));}
        receipt.aperture=aperture;
        receipt.construction=ApertureConstruction::MacroFacet(MacroFacetClipSource{triangle,vertex_ids,regions,cell,bounds,patches});
        Ok(receipt)
    }
    pub(crate) fn replay(&self) -> CaeResult<Vec<[S;3]>> {
        if let Some(f)=self.macro_facet(){
            let mut polygon=f.triangle.to_vec();
            for axis in 0..3{for side in 0..2{
                let plane=Plane{normal:std::array::from_fn(|k|S::from_f64(if k==axis{if side==0{1.}else{-1.}}else{0.})),origin:f.bounds[side].map(S::from_f64),offset:S::zero(),boundary:CutBoundary::Cell{axis,side}};
                polygon=geometry::clip_polygon(&polygon,&plane,self.eps)?;
            }}
            return Ok(polygon);
        }
        let a=self.source.get(self.left_face).ok_or_else(||fail("aperture source face is absent"))?;
        let b=self.target.get(self.right_face).ok_or_else(||fail("aperture target face is absent"))?;
        geometry::polygon_part_intersection(a,b,&self.target,self.eps)
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
                let order = tet.vertices_m[i][k]
                    .value()
                    .total_cmp(&tet.vertices_m[j][k].value());
                if !order.is_eq() {
                    return order;
                }
            }
            std::cmp::Ordering::Equal
        });
    }
    Ok(indices.map(|i| tet.vertices_m[i]))
}
pub(crate) fn zero_area_numerical_aperture<S: Scalar>(
    a: &CutPolygon<S>,
    b: &CutPolygon<S>,
    source: &[CutPolygon<S>],
    target: &[CutPolygon<S>],
    faces: &BTreeMap<usize, Option<[[usize; 3]; 4]>>,
    vertices: &BTreeMap<usize, [S; 3]>,
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
        let point = *vertices
            .get(&id)
            .ok_or_else(|| fail("numerical aperture face vertex is missing"))?;
        for facet in [a, b] {
            let d =
                dot(facet.outward_normal, sub(point, facet.plane_origin_m)) + facet.plane_offset_m;
            let scale = (0..3)
                .map(|k| {
                    facet.outward_normal[k].value().abs()
                        * (point[k].value().abs() + facet.plane_origin_m[k].value().abs())
                })
                .sum::<f64>()
                + facet.plane_offset_m.value().abs();
            if !d.value().is_finite() || !scale.is_finite() {
                return Err(fail("numerical aperture face incidence overflow"));
            }
            if d.value().abs() > 128. * f64::EPSILON * scale {
                return Ok(false);
            }
        }
    }
    for (facet, constraints) in [(a, source), (b, target)] {
        for plane in constraints {
            for point in &facet.vertices_m {
                let d = dot(plane.outward_normal, sub(*point, plane.plane_origin_m))
                    + plane.plane_offset_m;
                let scale = (0..3)
                    .map(|k| {
                        plane.outward_normal[k].value().abs()
                            * (point[k].value().abs() + plane.plane_origin_m[k].value().abs())
                    })
                    .sum::<f64>()
                    + plane.plane_offset_m.value().abs();
                if !d.value().is_finite() || !scale.is_finite() {
                    return Err(fail("numerical aperture containment overflow"));
                }
                if d.value() > 128. * f64::EPSILON * scale {
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
        let mut points = [[S::zero(); 3]; 2];
        for (k, id) in edge.iter().enumerate() {
            points[k] = *vertices
                .get(id)
                .ok_or_else(|| fail("numerical aperture edge vertex is missing"))?;
        }
        let incident = points.iter().all(|point| {
            let d =
                dot(plane.outward_normal, sub(*point, plane.plane_origin_m)) + plane.plane_offset_m;
            let scale = (0..3)
                .map(|k| {
                    plane.outward_normal[k].value().abs()
                        * (point[k].value().abs() + plane.plane_origin_m[k].value().abs())
                })
                .sum::<f64>()
                + plane.plane_offset_m.value().abs();
            d.value().is_finite()
                && scale.is_finite()
                && d.value().abs() <= 128. * f64::EPSILON * scale
        });
        if !incident {
            continue;
        }
        let projected = cross(sub(points[1], points[0]), a.outward_normal);
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
        if orientation.abs() <= sign_roundoff {
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
fn tetra_planes<S: Scalar>(tet: &CutTetraSubdomain<S>) -> CaeResult<(Vec<Plane<S>>, S)> {
    let p = tet.vertices_m;
    let determinant = dot(sub(p[1], p[0]), cross(sub(p[2], p[0]), sub(p[3], p[0])));
    if !determinant.value().is_finite() || determinant.value() <= 0. {
        return Err(fail("refinement tetrahedron must have positive volume"));
    }
    let mut planes = vec![];
    for (face, f) in [[0, 1, 2, 3], [0, 1, 3, 2], [0, 2, 3, 1], [1, 2, 3, 0]]
        .into_iter()
        .enumerate()
    {
        let canonical = canonical_face(tet, face)?;
        let n = cross(
            sub(canonical[1], canonical[0]),
            sub(canonical[2], canonical[0]),
        );
        let length = dot(n, n).sqrt();
        let mut n = n.map(|v| v / length);
        if dot(n, sub(p[f[3]], canonical[0])).value() < 0. {
            n = n.map(|v| -v);
        }
        planes.push(Plane {
            normal: n,
            origin: canonical[0],
            offset: S::zero(),
            boundary: CutBoundary::Subdomain {
                region: tet.region,
                face,
            },
        });
    }
    Ok((planes, determinant / 6.))
}
fn tetra_poly<S: Scalar>(tet: &CutTetraSubdomain<S>, planes: &[Plane<S>]) -> Vec<CutPolygon<S>> {
    [[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]]
        .into_iter()
        .enumerate()
        .map(|(i, f)| CutPolygon {
            support_only: false,
            vertices_m: f.map(|j| tet.vertices_m[j]).to_vec(),
            outward_normal: planes[i].normal.map(|v| -v),
            plane_origin_m: planes[i].origin,
            plane_offset_m: -planes[i].offset,
            boundary: planes[i].boundary.clone(),
        })
        .collect()
}
fn disjoint<S: Scalar>(a: &[[S; 3]], b: &[[S; 3]]) -> bool {
    (0..3).any(|k| {
        let al = a.iter().map(|p| p[k].value()).fold(f64::INFINITY, f64::min);
        let ah = a
            .iter()
            .map(|p| p[k].value())
            .fold(f64::NEG_INFINITY, f64::max);
        let bl = b.iter().map(|p| p[k].value()).fold(f64::INFINITY, f64::min);
        let bh = b
            .iter()
            .map(|p| p[k].value())
            .fold(f64::NEG_INFINITY, f64::max);
        ah <= bl || bh <= al
    })
}
fn intersection<S: Scalar>(
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
fn has_volume<S: Scalar>(p: &[CutPolygon<S>], eps: f64) -> CaeResult<bool> {
    Ok(geometry::measure(p)?.0.value() > 0.
        && !geometry::dimensionless_zero(
            &p.iter()
                .flat_map(|f| &f.vertices_m)
                .copied()
                .collect::<Vec<_>>(),
            eps,
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
    let mut native_vertices: BTreeMap<usize, [S; 3]> = BTreeMap::new();
    let mut numerical_faces = BTreeMap::new();
    for tet in tets {
        let keys = tet
            .vertex_ids
            .map(|ids| [[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]].map(|f| f.map(|i| ids[i])));
        if numerical_faces.insert(tet.region, keys).is_some() {
            return Err(fail("duplicate numerical gas region identifier"));
        }
        if let Some(ids) = tet.vertex_ids {
            for (i, id) in ids.into_iter().enumerate() {
                if let Some(previous) = native_vertices.get(&id) {
                    if (0..3).any(|k| {
                        previous[k].value() != tet.vertices_m[i][k].value()
                            || (previous[k] - tet.vertices_m[i][k]).has_direction()
                    }) {
                        return Err(fail("shared refinement vertex has inconsistent geometry"));
                    }
                } else {
                    native_vertices.insert(id, tet.vertices_m[i]);
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
                if disjoint(
                    &p.iter()
                        .flat_map(|f| &f.vertices_m)
                        .copied()
                        .collect::<Vec<_>>(),
                    &tets[i].vertices_m,
                ) {
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
        let surface_area: f64 = tetra_poly(&tets[i], planes)
            .iter()
            .map(|f| geometry::area(&f.vertices_m).value())
            .sum();
        let bound = 2048. * f64::EPSILON * volume.value() + 8. * eps * surface_area;
        maxbound = maxbound.max(bound);
        if volume.value() <= bound {
            return Err(fail(
                "refinement tetrahedron is below arithmetic volume resolution",
            ));
        }
        if defect > bound {
            return Err(fail(&format!(
                "refinement tetrahedron {i} is not wholly owned by sharp void volume {:e} covered {:e} defect {:e}",
                volume.value(),
                covered.value(),
                defect
            )));
        }
        for j in 0..i {
            if disjoint(&tets[i].vertices_m, &tets[j].vertices_m) {
                continue;
            }
            let p = tetra_poly(&tets[i], planes);
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
                    if disjoint(
                        &part
                            .iter()
                            .flat_map(|f| &f.vertices_m)
                            .copied()
                            .collect::<Vec<_>>(),
                        &tets[i].vertices_m,
                    ) {
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
                        if geometry::positive_polygon(
                            &geometry::polygon_intersection(a, b, eps)?,
                            eps,
                        ) {
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
        components.push(CutCellComponent {
            cell,
            component: *id,
            volume_m3: volume,
            centroid_m: centroid,
            central_second_integral_m5: geometry::central_second_integral(&parts, centroid)?,
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
        components_by_cell.entry(component.cell).or_default().push(index);
    }
    let mut adjacent_cells: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for &[a, b] in &neighbours {
        adjacent_cells.entry(a).or_default().insert(b);
        adjacent_cells.entry(b).or_default().insert(a);
    }
    let mut external_by_cell: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (index, aperture) in base.apertures.iter().enumerate() {
        if aperture.right.is_none() {
            let cell = base.components.get(aperture.left)
                .ok_or_else(|| fail("external aperture owner is missing"))?.cell;
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
                    let polygon = geometry::polygon_part_intersection(a, b, part, eps)?;
                    if geometry::positive_polygon(&polygon, eps) {
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
                            area_m2: geometry::area(&polygon),
                            polygons: vec![polygon],
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
                            left:i,right:j,
                            left_part:components[i].convex_parts.iter().position(|p|std::ptr::eq(p,source)).unwrap(),
                            right_part:components[j].convex_parts.iter().position(|p|std::ptr::eq(p,part)).unwrap(),
                            left_face:source.iter().position(|f|std::ptr::eq(f,a)).unwrap(),
                            right_face:part.iter().position(|f|std::ptr::eq(f,b)).unwrap(),
                            source:source.clone(),target:part.clone(),
                            aperture:apertures.last().unwrap().clone(),eps,construction:ApertureConstruction::ConvexClip,
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
                for p in &external.polygons {
                    let reference = CutPolygon {
                        support_only: false,
                        vertices_m: p.clone(),
                        outward_normal: external.normal,
                        plane_origin_m: p[0],
                        plane_offset_m: S::zero(),
                        boundary: CutBoundary::Cell {
                            axis: external.axis.unwrap(),
                            side: 0,
                        },
                    };
                    if dot(face.outward_normal, external.normal).value() < 0.5 {
                        continue;
                    }
                    let polygon = geometry::polygon_intersection(face, &reference, eps)?;
                    if geometry::positive_polygon(&polygon, eps) {
                        apertures.push(CutCellAperture {
                            left: i,
                            right: None,
                            outside_cell: external.outside_cell,
                            axis: external.axis,
                            normal: face.outward_normal,
                            area_m2: geometry::area(&polygon),
                            polygons: vec![polygon],
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
                let reference = CutPolygon {
                    support_only: false,
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
                let polygon = geometry::polygon_intersection(face, &reference, eps)?;
                if !geometry::positive_polygon(&polygon, eps) {
                    continue;
                }
                let p = wall.vertices_m[0];
                let mut best = (0., 0, 0);
                for a in 1..wall.vertices_m.len() {
                    for b in a + 1..wall.vertices_m.len() {
                        let u = sub(wall.vertices_m[a], p);
                        let v = sub(wall.vertices_m[b], p);
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
                let u = sub(wall.vertices_m[best.1], p);
                let v = sub(wall.vertices_m[best.2], p);
                let n = cross(u, v);
                let denominator = dot(n, n);
                let bary = polygon
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
                    area_m2: geometry::area(&polygon),
                    normal: wall.normal,
                    vertices_m: polygon,
                    native_nodes: wall.native_nodes,
                    native_barycentric: bary,
                });
            }
        }
    }
    let total_volume_m3: S = components.iter().map(|c| c.volume_m3).fold(S::zero(), |a,b|a+b);
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
