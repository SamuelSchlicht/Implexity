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

use super::{corner_replay, NativeGasConvexSupport, NativeGasGeometryAdapter};
use crate::gas_geometry_time::{GeometryTime, GeometryTimeBase};
use crate::gas_transient_kinetic::TransientGasScalar;
use crate::native_cut_cell_partition::{CutPolygon, CutWallQuadrature};
use crate::native_geometry_construction_error::{ConstructionScope, ConvexSupportConstructionProof, JetLayout, TrackedScalar};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

fn fail(s: &str) -> CaeError { CaeError::contract(s) }

pub(super) fn issue<S: GeometryTimeBase>(
    adapter: &NativeGasGeometryAdapter,
    support: &NativeGasConvexSupport<GeometryTime<S>>,
    trace: &[GeometryTime<S>], design: &[GeometryTime<S>],
    quadrature: &CutWallQuadrature<GeometryTime<S>>, fan: usize,
    current_centroid: [S; 3],
) -> CaeResult<ConvexSupportConstructionProof> {
    let width = trace.iter().chain(design).filter_map(|v|v.geometry_first_order_directions())
        .map(|v|v.len()).max().unwrap_or(1);
    if width == 0 || width % 2 != 1 { return Err(fail("wall construction mixed layout is invalid")); }
    let layout = JetLayout::TimeMixed((width-1)/2);
    if quadrature.component != support.component || support.corner_constructions.len() != support.polygons.len() {
        return Err(fail("wall construction support binding differs"));
    }
    let owner = adapter.owner.capacity_owner();
    let mut point_proofs = Vec::new();
    for (polygon, recipes) in support.polygons.iter().zip(&support.corner_constructions) {
        if polygon.vertices_m.len() != recipes.len() { return Err(fail("wall construction recipe dimension differs")); }
        let mut proofs = Vec::new();
        for recipe in recipes {
            let p = corner_replay::issue(recipe,owner,&adapter.kinematics,adapter.layers,trace,design,layout)?;
            if !p.certifies_active_constraints() { return Err(fail("wall construction active support is unproved")); }
            proofs.push(p.point().clone());
        }
        point_proofs.push(proofs);
    }
    let mut wall_proofs = Vec::new();
    for recipe in &support.wall_constructions {
        let p = corner_replay::issue(recipe,owner,&adapter.kinematics,adapter.layers,trace,design,layout)?;
        if !p.certifies_active_constraints() { return Err(fail("wall quadrature active support is unproved")); }
        wall_proofs.push(p.point().clone());
    }
    if fan+2 >= wall_proofs.len() { return Err(fail("wall construction fan ownership is absent")); }
    let _scope = ConstructionScope::new(layout)?;
    let trace: Vec<_> = trace.iter().copied().map(TrackedScalar::from_control).collect();
    let design: Vec<_> = design.iter().copied().map(TrackedScalar::from_control).collect();
    let all_recipes: Vec<_> = support.corner_constructions.iter().flatten().chain(&support.wall_constructions).collect();
    let charts = std::cell::RefCell::new(std::collections::BTreeMap::new());
    let layered = |id: usize| {
        let source = all_recipes.iter().flat_map(|c| &c.layered_sources).chain(&support.plane_layered_sources).find(|v|v.0==id)
            .ok_or_else(||fail("wall construction layered feature is absent"))?;
        let (_, chart, corner, layer) = *source;
        if adapter.layers == 0 || layer > adapter.layers || corner >= 3 { return Err(fail("wall construction layered feature is invalid")); }
        if !charts.borrow().contains_key(&chart) {
            let p = adapter.kinematics.mapped_chart_from_trace(chart,&trace,&design,|p,_|Ok(p))?;
            charts.borrow_mut().insert(chart,p);
        }
        let p = *charts.borrow().get(&chart).ok_or_else(||fail("wall construction chart is absent"))?;
        Ok(std::array::from_fn(|k|p[corner][k]+(p[corner+3][k]-p[corner][k])*(layer as f64/adapter.layers as f64)))
    };
    let mut polygons = Vec::new();
    for ((polygon, recipes), proofs) in support.polygons.iter().zip(&support.corner_constructions).zip(&point_proofs) {
        let cell = recipes.first().or_else(||support.wall_constructions.first())
            .ok_or_else(||fail("wall construction cell recipe is absent"))?.cell;
        let (mut n,o,mut b) = corner_replay::native_plane(owner,cell,&polygon.boundary,&trace,&design,|region| {
            let ids = all_recipes.iter().flat_map(|c| &c.tet_vertices).chain(&support.plane_tet_vertices)
                .find(|v|v.0==region).ok_or_else(||fail("wall construction tet feature is absent"))?.1;
            let [a,b,c,d] = ids.map(&layered);
            Ok(crate::native_cut_cell_refinement::CutTetraSubdomain{region,vertex_ids:Some(ids),vertices_m:[a?,b?,c?,d?]})
        })?;
        let orientation = (0..3).map(|k|n[k].value()*polygon.outward_normal[k].value()).sum::<f64>();
        let scale=TrackedScalar::from_f64(if orientation < 0. {-1.}else{1.});
        n=n.map(|v|v*scale);b=b*scale;
        for k in 0..3 {
            let proof=n[k].construction_proof()?;
            TrackedScalar::from_bound(polygon.outward_normal[k],&proof).map_err(|e|fail(&format!("wall support normal binding axis={k} boundary={:?}: {e}",polygon.boundary)))?;
            let proof=o[k].construction_proof()?;
            TrackedScalar::from_bound(polygon.plane_origin_m[k],&proof).map_err(|e|fail(&format!("wall support origin binding axis={k} boundary={:?}: {e}",polygon.boundary)))?;
        }
        TrackedScalar::from_bound(polygon.plane_offset_m,&b.construction_proof()?).map_err(|e|fail(&format!("wall support offset binding boundary={:?}: {e}",polygon.boundary)))?;
        let mut vertices=Vec::new();
        for (point,proof) in polygon.vertices_m.iter().zip(proofs) {
            vertices.push([TrackedScalar::from_bound(point[0],&proof[0])?,TrackedScalar::from_bound(point[1],&proof[1])?,TrackedScalar::from_bound(point[2],&proof[2])?]);
        }
        polygons.push(CutPolygon{support_only:polygon.support_only,vertices_m:vertices,outward_normal:n,plane_origin_m:o,plane_offset_m:b,boundary:polygon.boundary.clone()});
    }
    let (volume,moment,centroid)=measure_native_parent_bound(adapter,support,&polygons)?;
    volume.construction_proof().map_err(|e|fail(&format!("wall support volume construction: {e}")))?;
    for c in centroid {c.construction_proof().map_err(|e|fail(&format!("wall support centroid construction: {e}")))?;}
    let ids=[0,fan+1,fan+2];
    let mut p=Vec::new();
    for id in ids {
        let point=support.wall_constructions[id].actual;
        let proof=&wall_proofs[id];
        p.push([TrackedScalar::from_bound(point[0],&proof[0])?,TrackedScalar::from_bound(point[1],&proof[1])?,TrackedScalar::from_bound(point[2],&proof[2])?]);
    }
    let a:[_;3]=std::array::from_fn(|k|p[1][k]-p[0][k]);
    let b:[_;3]=std::array::from_fn(|k|p[2][k]-p[0][k]);
    let cross=[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]];
    let vector=cross.map(|v|TrackedScalar::zero()+v*0.5);
    let area=crate::native_cut_cell_partition::area(&p);
    let sign=if (0..3).map(|k|vector[k].value()*quadrature.normal[k].value()).sum::<f64>() < 0. {-1.}else{1.};
    let normal=vector.map(|v|v*sign/area);
    let q:[_;3]=std::array::from_fn(|k|(0..3).map(|j|p[j][k]*(1./3.)).fold(TrackedScalar::zero(),|a,b|a+b));
    for k in 0..3 {
        TrackedScalar::from_bound(quadrature.position_m[k],&q[k].construction_proof()?).map_err(|e|fail(&format!("wall quadrature position binding axis={k}: {e}")))?;
        TrackedScalar::from_bound(quadrature.normal[k],&normal[k].construction_proof()?).map_err(|e|fail(&format!("wall quadrature normal binding axis={k}: {e}")))?;
    }
    let position=std::array::from_fn(|k|q[k].recompose_face(GeometryTime::new(current_centroid[k],S::zero())));
    let vertices:Vec<_>=polygons.iter().flat_map(|p|p.vertices_m.iter().copied()).collect();
    let planes:Vec<_>=polygons.iter().map(|p|(p.outward_normal,p.plane_origin_m,p.plane_offset_m)).collect();
    let owned=adapter.issue_native_owned_wall_facet(support,quadrature,fan)?;
    ConvexSupportConstructionProof::from_tracked(volume,centroid,&vertices,&planes,position,normal).and_then(|proof|proof.with_owned_native_wall_facet(&owned)).map_err(|e|fail(&format!("wall support residual construction vertices={} planes={}: {e}",vertices.len(),planes.len())))
}


pub(crate) fn centred_centroid_bound<S: GeometryTimeBase>(faces:&[CutPolygon<TrackedScalar<GeometryTime<S>>>],volume:TrackedScalar<GeometryTime<S>>,moment:[TrackedScalar<GeometryTime<S>>;3])->CaeResult<[TrackedScalar<GeometryTime<S>>;3]>{
    let count=faces.iter().filter(|f|!f.support_only).map(|f|f.vertices_m.len()).sum::<usize>();
    if count==0{return Err(fail("wall centroid construction has no volume vertices"));}
    let centre:[_;3]=std::array::from_fn(|k|faces.iter().filter(|f|!f.support_only).flat_map(|f|&f.vertices_m).map(|p|p[k]).fold(TrackedScalar::zero(),|a,b|a+b)/count as f64);
    let mut local=[TrackedScalar::zero();3];
    for f in faces.iter().filter(|f|!f.support_only){for i in 1..f.vertices_m.len().saturating_sub(1){
        let a:[_;3]=std::array::from_fn(|k|f.vertices_m[0][k]-centre[k]);let b:[_;3]=std::array::from_fn(|k|f.vertices_m[i][k]-centre[k]);let c:[_;3]=std::array::from_fn(|k|f.vertices_m[i+1][k]-centre[k]);
        let cross=[b[1]*c[2]-b[2]*c[1],b[2]*c[0]-b[0]*c[2],b[0]*c[1]-b[1]*c[0]];
        let dv=(a[0]*cross[0]+a[1]*cross[1]+a[2]*cross[2]).abs()/6.;
        for k in 0..3{local[k]+=dv*(a[k]+b[k]+c[k])*0.25;}
    }}
    let mut result=[TrackedScalar::zero();3];
    for k in 0..3{let observed=moment[k]/volume;let shifted=centre[k]+local[k]/volume;let observed_bits=observed.construction_proof()?.observed_bits();let channels=observed_bits.into_iter().map(f64::from_bits).collect::<Vec<_>>();let proof=shifted.construction_proof()?.reconcile(&channels).map_err(CaeError::contract)?;result[k]=TrackedScalar::from_bound(observed.actual(),&proof)?;}
    Ok(result)
}

fn measure_native_parent_bound<S:GeometryTimeBase>(adapter:&NativeGasGeometryAdapter,support:&NativeGasConvexSupport<GeometryTime<S>>,faces:&[CutPolygon<TrackedScalar<GeometryTime<S>>>])->CaeResult<(TrackedScalar<GeometryTime<S>>,[TrackedScalar<GeometryTime<S>>;3],[TrackedScalar<GeometryTime<S>>;3])>{
    let count=faces.iter().filter(|f|!f.support_only).map(|f|f.vertices_m.len()).sum::<usize>();
    if count==0{return Err(fail("wall volume construction has no vertices"));}
    let centre:[_;3]=std::array::from_fn(|k|faces.iter().filter(|f|!f.support_only).flat_map(|f|&f.vertices_m).map(|p|p[k]).fold(TrackedScalar::zero(),|a,b|a+b)/count as f64);
    let mut volume=TrackedScalar::zero();let mut moment=[TrackedScalar::zero();3];let mut local=[TrackedScalar::zero();3];
    for (face,f) in faces.iter().enumerate().filter(|(_,f)|!f.support_only){for i in 1..f.vertices_m.len().saturating_sub(1){
        let points=[centre,f.vertices_m[0],f.vertices_m[i],f.vertices_m[i+1]];
        let a:[_;3]=std::array::from_fn(|k|points[1][k]-centre[k]);let b:[_;3]=std::array::from_fn(|k|points[2][k]-centre[k]);let c:[_;3]=std::array::from_fn(|k|points[3][k]-centre[k]);
        let cross=[b[1]*c[2]-b[2]*c[1],b[2]*c[0]-b[0]*c[2],b[0]*c[1]-b[1]*c[0]];
        let signed=(0..3).map(|k|a[k]*cross[k]).fold(TrackedScalar::zero(),|a,b|a+b);
        let absolute=if let Some(owner)=adapter.issue_native_source_zero_fan(support,face,i)?{TrackedScalar::from_native_source_zero_fan(signed.actual().abs(),&owner)?}else{signed.abs()};
        let dv=absolute/6.;dv.construction_proof().map_err(|e|fail(&format!("native wall fan construction face={face} fan={i} boundary={:?}: {e}",f.boundary)))?;volume+=dv;
        for k in 0..3{moment[k]+=dv*(points[0][k]+points[1][k]+points[2][k]+points[3][k])*0.25;local[k]+=dv*(a[k]+b[k]+c[k])*0.25;}
    }}
    let mut centroid=[TrackedScalar::zero();3];
    for k in 0..3{let observed=moment[k]/volume;let shifted=centre[k]+local[k]/volume;let observed_bits=observed.construction_proof()?.observed_bits();let channels=observed_bits.into_iter().map(f64::from_bits).collect::<Vec<_>>();let proof=shifted.construction_proof()?.reconcile(&channels).map_err(CaeError::contract)?;centroid[k]=TrackedScalar::from_bound(observed.actual(),&proof)?;}
    Ok((volume,moment,centroid))
}
