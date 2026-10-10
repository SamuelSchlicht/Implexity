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
use crate::gas_transient_kinetic::TransientGasScalar;

#[derive(Clone, Debug)]
pub struct PhysicalFacet<S: Scalar> {
    pub owner: Vec<usize>,
    pub vertices: [[S; 3]; 3],
    pub vertex_features: [Vec<usize>; 3],
}

#[derive(Clone, Debug)]
pub struct PhysicalBoundaryPoint<S: Scalar> {
    pub feature: Vec<usize>,
    pub point: [S; 3],
}

#[derive(Clone, Debug)]
pub struct PhysicalBoundarySegment<S: Scalar> {
    pub owner: Vec<usize>,
    pub endpoints: [PhysicalBoundaryPoint<S>; 2],
}

#[derive(Clone, Debug)]
pub enum PhysicalGridSection<S: Scalar> {
    Transverse(PhysicalBoundarySegment<S>),
    Coplanar(PhysicalFacet<S>),
}

pub fn physical_grid_section<S: Scalar>(
    facet: &PhysicalFacet<S>, axis: usize, coordinate: f64,
    plane_owner: &[usize], same_motion: impl Fn(S, S) -> bool,
) -> Result<Option<PhysicalGridSection<S>>, String> {
    if axis >= 3 || !coordinate.is_finite() || plane_owner.is_empty() || facet.owner.is_empty()
        || facet.vertex_features.iter().any(Vec::is_empty)
        || facet.vertices.iter().flatten().any(|v|!v.value().is_finite()) {
        return Err("physical grid section ownership is absent".into());
    }
    if facet.vertices.iter().all(|point| point[axis].value() == coordinate) {
        if facet.vertices.iter().any(|point| !same_motion(point[axis]-coordinate,S::zero())) {
            return Err("coplanar physical grid footprint has uncertified normal motion".into());
        }
        return Ok(Some(PhysicalGridSection::Coplanar(facet.clone())));
    }
    physical_facet_section(facet, axis, coordinate, plane_owner, same_motion)
        .map(|section| section.map(PhysicalGridSection::Transverse))
}

pub fn clip_physical_segment<S: TransientGasScalar>(
    segment: &PhysicalBoundarySegment<S>, section_axis: usize,
    bounds: [[f64; 3]; 2], cell: usize,
) -> Result<Option<PhysicalBoundarySegment<S>>, String> {
    if section_axis >= 3 || bounds.iter().flatten().any(|value| !value.is_finite()) {
        return Err("invalid authentic physical segment cell bounds".into());
    }
    let a = &segment.endpoints[0]; let b = &segment.endpoints[1];
    let mut first = S::zero(); let mut last = S::one();
    let mut first_owner = None; let mut last_owner = None;
    for axis in (0..3).filter(|&axis| axis != section_axis) {
        if bounds[0][axis] >= bounds[1][axis] {
            return Err("empty physical segment cell bounds".into());
        }
        let delta = b.point[axis] - a.point[axis];
        if delta.value() == 0. {
            if a.point[axis].value() < bounds[0][axis] || a.point[axis].value() > bounds[1][axis] {return Ok(None);}
            if a.point[axis].value()==bounds[0][axis] || a.point[axis].value()==bounds[1][axis] {return Err("coplanar physical segment cell edge needs an authenticated one-sided owner".into());}
            continue;
        }
        let lower = (S::from_f64(bounds[0][axis]) - a.point[axis]) / delta;
        let upper = (S::from_f64(bounds[1][axis]) - a.point[axis]) / delta;
        let (entry,exit,entry_side,exit_side) = if delta.value() > 0. {(lower,upper,0,1)}else{(upper,lower,1,0)};
        let maintained=|difference:S|difference.value()==0.&&difference.geometry_first_order_directions().is_some_and(|v|v.iter().all(|d|*d==0.));
        if entry.value()==first.value() && !maintained(entry-first) {return Err("physical segment entry reaches a non-maintained clipping incidence".into());}
        if exit.value()==last.value() && !maintained(exit-last) {return Err("physical segment exit reaches a non-maintained clipping incidence".into());}
        if entry.value() > first.value() {first=entry;first_owner=Some((axis,entry_side));}
        if exit.value() < last.value() {last=exit;last_owner=Some((axis,exit_side));}
        if first.value() > last.value() {return Ok(None);}
        if first.value() == last.value() {return Err("physical segment touches a cell clipping event".into());}
    }
    let endpoint = |parameter:S, owner:Option<(usize,usize)>, parent:&PhysicalBoundaryPoint<S>| {
        if let Some((axis,side))=owner {
            PhysicalBoundaryPoint {
                feature:joined_feature(2,&a.feature,&b.feature,&[cell,axis,side]),
                point:std::array::from_fn(|k|if k==axis {S::from_f64(bounds[side][axis])}else{a.point[k]+(b.point[k]-a.point[k])*parameter}),
            }
        } else {parent.clone()}
    };
    Ok(Some(PhysicalBoundarySegment {owner:segment.owner.clone(),endpoints:[endpoint(first,first_owner,a),endpoint(last,last_owner,b)]}))
}

fn joined_feature(kind: usize, a: &[usize], b: &[usize], plane: &[usize]) -> Vec<usize> {
    let (a, b) = if a <= b { (a, b) } else { (b, a) };
    [vec![kind, a.len()], a.to_vec(), vec![b.len()], b.to_vec(), vec![plane.len()], plane.to_vec()].concat()
}

pub fn physical_facet_section<S: Scalar>(
    facet: &PhysicalFacet<S>,
    axis: usize,
    coordinate: f64,
    plane_owner: &[usize],
    same_motion: impl Fn(S, S) -> bool,
) -> Result<Option<PhysicalBoundarySegment<S>>, String> {
    if axis >= 3 || !coordinate.is_finite() || plane_owner.is_empty() || facet.owner.is_empty()
        || facet.vertex_features.iter().any(Vec::is_empty)
    {
        return Err("physical facet/grid section ownership is absent".into());
    }
    let distances = facet.vertices.map(|p| p[axis] - coordinate);
    if distances.iter().all(|d| d.value() == 0.) {
        return Err("coplanar physical facet/grid interface needs a one-sided footprint owner".into());
    }
    let mut points = Vec::<PhysicalBoundaryPoint<S>>::new();
    for i in 0..3 {
        if distances[i].value() == 0. {
            if !same_motion(distances[i], S::zero()) {
                return Err("physical native vertex crosses the grid section stratum".into());
            }
            points.push(PhysicalBoundaryPoint {
                feature: joined_feature(0, &facet.vertex_features[i], &[], plane_owner),
                point: facet.vertices[i],
            });
        }
        let j = (i + 1) % 3;
        if (distances[i].value()>0.&&distances[j].value()<0.)||(distances[i].value()<0.&&distances[j].value()>0.) {
            let [a,b]=if facet.vertex_features[i] <= facet.vertex_features[j] {[i,j]}else{[j,i]};
            let fraction = distances[a] / (distances[a] - distances[b]);
            points.push(PhysicalBoundaryPoint {
                feature: joined_feature(1, &facet.vertex_features[i], &facet.vertex_features[j], plane_owner),
                point: std::array::from_fn(|k| {
                    if k == axis { S::from_f64(coordinate) }
                    else { facet.vertices[a][k] + (facet.vertices[b][k] - facet.vertices[a][k]) * fraction }
                }),
            });
        }
    }
    if points.len() < 2 {
        return Ok(None);
    }
    if points.len() != 2 {
        return Err("physical facet/grid section is not a unique segment".into());
    }
    let a = std::array::from_fn::<_, 3, _>(|k| facet.vertices[1][k] - facet.vertices[0][k]);
    let b = std::array::from_fn::<_, 3, _>(|k| facet.vertices[2][k] - facet.vertices[0][k]);
    let normal = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let transverse = [ (axis + 1) % 3, (axis + 2) % 3 ];
    let orientation = (points[1].point[transverse[0]] - points[0].point[transverse[0]]) * normal[transverse[1]]
        - (points[1].point[transverse[1]] - points[0].point[transverse[1]]) * normal[transverse[0]];
    if orientation.value() == 0. || !orientation.value().is_finite() {
        return Err("physical facet/grid section loses transverse rank".into());
    }
    if orientation.value() < 0. { points.swap(0, 1); }
    Ok(Some(PhysicalBoundarySegment {
        owner: facet.owner.clone(),
        endpoints: [points.remove(0), points.remove(0)],
    }))
}
