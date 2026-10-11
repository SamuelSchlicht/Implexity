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
use crate::native_cut_cell_partition::CutPolygon;
use crate::native_geometry_construction_error::{ConstructionScope, ConvexSupportConstructionProof, JetLayout, TrackedScalar};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

fn fail(s: &str) -> CaeError { CaeError::contract(s) }

pub(super) fn issue_source<S: GeometryTimeBase>(
    adapter:&NativeGasGeometryAdapter,
    support:&NativeGasConvexSupport<GeometryTime<S>>,
    labels:&crate::native_source_stage_labels::PreparedSourceStageLabels<'_,'_,'_,S>,
    window:usize,
    sample:&crate::native_physical_plane_preparation::PhysicalPlaneSample<GeometryTime<S>,crate::native_source_stage_labels::SourceFaceLabel>,
    _current_centroid:[S;3],
)->CaeResult<ConvexSupportConstructionProof>{
    let source=labels.source();if !std::ptr::eq(source.adapter(),adapter){return Err(fail("source support adapter differs"));}
    let token=match &sample.label{crate::native_source_stage_labels::SourceFaceLabel::NativeWallOwned(token)=>token,_=>return Err(fail("source support quadrature lacks owned wall authority"))};
    let(_,component,part,plane)=labels.validate_native_wall_owned(token,window,sample.source_uv,sample.position_m,sample.normal,&sample.source_receipt)?;
    if component!=support.component||part!=support.convex_part||plane>=support.polygons.len(){return Err(fail("source support component or part differs"));}
    labels.validate_native_wall_support(token,window,sample,&support.polygons[plane])?;
    let trace=source.live_stage_trace();let design=source.live_stage_design();
    let width = trace.iter().chain(design).filter_map(|v|v.geometry_first_order_directions())
        .map(|v|v.len()).max().unwrap_or(1);
    if width == 0 || width % 2 != 1 { return Err(fail("wall construction mixed layout is invalid")); }
    let layout = JetLayout::TimeMixed((width-1)/2);
    if support.corner_constructions.len() != support.polygons.len() {
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
    let _scope = ConstructionScope::new(layout)?;
    let trace: Vec<_> = trace.iter().copied().map(TrackedScalar::from_control).collect();
    let design: Vec<_> = design.iter().copied().map(TrackedScalar::from_control).collect();
    let all_recipes: Vec<_> = support.corner_constructions.iter().flatten().chain(&support.wall_constructions).collect();
    let charts = std::cell::RefCell::new(std::collections::BTreeMap::new());
    let replay_frame=std::cell::Cell::new(None::<[f64;3]>);
    let layered = |id: usize| {
        let source = all_recipes.iter().flat_map(|c| &c.layered_sources).chain(&support.plane_layered_sources).find(|v|v.0==id)
            .ok_or_else(||fail("wall construction layered feature is absent"))?;
        let (_, chart, corner, layer) = *source;
        if adapter.layers == 0 || layer > adapter.layers || corner >= 3 { return Err(fail("wall construction layered feature is invalid")); }
        if !charts.borrow().contains_key(&chart) {
            let p = adapter.kinematics.mapped_chart_from_trace(chart,&trace,&design,|p,_|Ok(p))?;
            charts.borrow_mut().insert(chart,p);
        }
        let p =if let Some(anchor)=replay_frame.get(){adapter.kinematics.mapped_chart_from_reference(chart,&trace,&design,|body,node|owner.node_reference_in_frame(body,node,anchor),|p,_|Ok(p))?}else{*charts.borrow().get(&chart).ok_or_else(||fail("wall construction chart is absent"))?};
        Ok(std::array::from_fn::<_,3,_>(|k|p[corner][k]+(p[corner+3][k]-p[corner][k])*(layer as f64/adapter.layers as f64)))
    };
    let mut polygons = Vec::new();
    for ((polygon, recipes), proofs) in support.polygons.iter().zip(&support.corner_constructions).zip(&point_proofs) {
        let cell = recipes.first().or_else(||support.wall_constructions.first())
            .ok_or_else(||fail("wall construction cell recipe is absent"))?.cell;
        let frame=polygon.local_geometry.as_ref().map(|g|g.anchor());
        replay_frame.set(frame);
        let (mut n,o,mut b) = corner_replay::native_plane_in_frame(owner,cell,&polygon.boundary,frame,&trace,&design,|region| {
            let ids = all_recipes.iter().flat_map(|c| &c.tet_vertices).chain(&support.plane_tet_vertices)
                .find(|v|v.0==region).ok_or_else(||fail("wall construction tet feature is absent"))?.1;
            let [a,b,c,d] = ids.map(&layered);
            Ok(crate::native_cut_cell_refinement::CutTetraSubdomain{region,vertex_ids:Some(ids),vertices_m:[a?,b?,c?,d?],local_geometry:None})
        })?;
        let orientation = (0..3).map(|k|n[k].value()*polygon.outward_normal[k].value()).sum::<f64>();
        let scale=TrackedScalar::from_f64(if orientation < 0. {-1.}else{1.});
        n=n.map(|v|v*scale);b=b*scale;
        for k in 0..3 {
            let proof=n[k].construction_proof()?;
            TrackedScalar::from_bound(polygon.outward_normal[k],&proof).map_err(|e|fail(&format!("wall support normal binding axis={k} boundary={:?}: {e}",polygon.boundary)))?;
            let proof=(o[k]+frame.map_or(0.,|a|a[k])).construction_proof()?;
            TrackedScalar::from_bound(polygon.plane_origin_m[k],&proof).map_err(|e|fail(&format!("wall support origin binding axis={k} boundary={:?}: {e}",polygon.boundary)))?;
        }
        TrackedScalar::from_bound(polygon.plane_offset_m,&b.construction_proof()?).map_err(|e|fail(&format!("wall support offset binding boundary={:?}: {e}",polygon.boundary)))?;
        let mut vertices=Vec::new();
        let points=polygon.local_geometry.as_ref().map_or(polygon.vertices_m.as_slice(),|g|g.vertices());
        for (point,proof) in points.iter().zip(proofs) {
            vertices.push([TrackedScalar::from_bound(point[0],&proof[0])?,TrackedScalar::from_bound(point[1],&proof[1])?,TrackedScalar::from_bound(point[2],&proof[2])?]);
        }
        let mut local_geometry=polygon.local_geometry.as_ref().map(|g|g.map_scalars(TrackedScalar::from_control));
        if let Some(local)=local_geometry.as_mut(){local.vertices_m=vertices.clone();local.plane_origin_m=o;}
        let vertices=vertices.into_iter().map(|p|std::array::from_fn(|k|p[k]+frame.map_or(0.,|a|a[k]))).collect();
        polygons.push(CutPolygon{support_only:polygon.support_only,vertices_m:vertices,outward_normal:n,plane_origin_m:std::array::from_fn(|k|o[k]+frame.map_or(0.,|a|a[k])),plane_offset_m:b,boundary:polygon.boundary.clone(),local_geometry});
    }
    replay_frame.set(None);
    let anchor=polygons.iter().find_map(|p|p.local_geometry.as_ref().map(|g|g.anchor())).ok_or_else(||fail("source support local frame is absent"))?;
    let local_polygons=polygons.iter().map(|p|{let mut view=p.clone();view.vertices_m=crate::native_cut_cell_partition::polygon_vertices_in_frame(p,anchor);view.plane_origin_m=crate::native_cut_cell_partition::polygon_plane_origin_in_frame(p,anchor);view.local_geometry=None;view}).collect::<Vec<_>>();
    let (volume,moment)=crate::native_cut_cell_partition::measure(&local_polygons)?;
    let centroid=moment.map(|v|v/volume);
    volume.construction_proof().map_err(|e|fail(&format!("wall support volume construction: {e}")))?;
    for c in centroid {c.construction_proof().map_err(|e|fail(&format!("wall support centroid construction: {e}")))?;}
    let facet=source.windows().get(window).ok_or_else(||fail("source support window is absent"))?.facet();
    let body=facet.owner.get(1).copied().filter(|b|*b<2).ok_or_else(||fail("source support native body is absent"))?;
    let node=|id:usize|->CaeResult<[TrackedScalar<GeometryTime<S>>;3]>{let n=owner.nodes[body].get(id).ok_or_else(||fail("source support native node is absent"))?;Ok(std::array::from_fn(|k|trace[n.force_flux[k]]+n.reference_m[k]))};
    let phase=|id:usize|->CaeResult<TrackedScalar<GeometryTime<S>>>{if id>=owner.nodes[body].len(){return Err(fail("source support phase node is absent"));}let row=id+if body==0{0}else{owner.nodes[0].len()};let(columns,weights)=owner.phase.row(row);Ok(columns.iter().zip(weights).map(|(&j,&w)|design[j]*w).fold(TrackedScalar::zero(),|a,b|a+b))};
    let mut corners=Vec::new();
    for (feature,actual) in facet.vertex_features.iter().zip(facet.vertices){let point=match feature.as_slice(){[0,b,id]if *b==body=>node(*id)?,[1,b,a,c]if *b==body&&a<c=>{let lower=node(*a)?;let upper=node(*c)?;let pa=phase(*a)?;let pc=phase(*c)?;let fraction=(TrackedScalar::from_f64(owner.iso)-pa)/(pc-pa);std::array::from_fn(|k|lower[k]+(upper[k]-lower[k])*fraction)},_=>return Err(fail("source support native feature recipe unsupported"))};for k in 0..3{TrackedScalar::from_bound(actual[k],&point[k].construction_proof()?).map_err(|e|fail(&format!("source Native corner binding window={window} feature={feature:?} axis={k} actual={:?} reconstructed={:?}: {e}",(actual[k].value(),actual[k].geometry_first_order_directions()),(point[k].actual().value(),point[k].actual().geometry_first_order_directions()))))?;}corners.push(point);}
    let origin=corners[0];let u=std::array::from_fn::<_,3,_>(|k|corners[1][k]-origin[k]);let v=std::array::from_fn::<_,3,_>(|k|corners[2][k]-origin[k]);
    let raw=[u[1]*v[2]-u[2]*v[1],u[2]*v[0]-u[0]*v[2],u[0]*v[1]-u[1]*v[0]];
    let jacobian=(0..3).map(|k|raw[k]*raw[k]).fold(TrackedScalar::zero(),|a,b|a+b).sqrt();
    let normal=crate::native_normalization::normalize_with_rebind(raw,jacobian,crate::native_normalization::NormalFormula::SourcePlane,TrackedScalar::rebind_owned_normal_chart)?.map(|n|-n);
    let uv=sample.source_uv.map(TrackedScalar::from_control);
    let q=std::array::from_fn::<_,3,_>(|k|origin[k]+u[k]*uv[0]+v[k]*uv[1]);
    for k in 0..3{TrackedScalar::from_bound(sample.position_m[k],&q[k].construction_proof()?).map_err(|e|fail(&format!("source q position binding window={window} axis={k} actual={:?} reconstructed={:?}: {e}",(sample.position_m[k].value(),sample.position_m[k].geometry_first_order_directions()),(q[k].actual().value(),q[k].actual().geometry_first_order_directions()))))?;TrackedScalar::from_bound(-sample.normal[k],&normal[k].construction_proof()?).map_err(|e|fail(&format!("source q normal binding window={window} axis={k} actual={:?} reconstructed={:?}: {e}",((-sample.normal[k]).value(),(-sample.normal[k]).geometry_first_order_directions()),(normal[k].actual().value(),normal[k].actual().geometry_first_order_directions()))))?;}
    let mut local_corners=Vec::new();
    let local_node=|id:usize|->CaeResult<[TrackedScalar<GeometryTime<S>>;3]>{let reference=owner.node_reference_in_frame(body,id,anchor)?;let n=&owner.nodes[body][id];Ok(std::array::from_fn(|k|trace[n.force_flux[k]]+reference[k]))};
    for feature in &facet.vertex_features{let point=match feature.as_slice(){[0,b,id]if *b==body=>local_node(*id)?,[1,b,a,c]if *b==body&&a<c=>{let lower=local_node(*a)?;let upper=local_node(*c)?;let pa=phase(*a)?;let pc=phase(*c)?;let fraction=(TrackedScalar::from_f64(owner.iso)-pa)/(pc-pa);std::array::from_fn(|k|lower[k]+(upper[k]-lower[k])*fraction)},_=>return Err(fail("source support local feature recipe is invalid"))};local_corners.push(point);}
    let local_q=std::array::from_fn::<_,3,_>(|k|local_corners[0][k]+(local_corners[1][k]-local_corners[0][k])*uv[0]+(local_corners[2][k]-local_corners[0][k])*uv[1]);
    let position=local_q;
    let vertices:Vec<_>=local_polygons.iter().flat_map(|p|p.vertices_m.iter().copied()).collect();let planes:Vec<_>=local_polygons.iter().map(|p|(p.outward_normal,p.plane_origin_m,p.plane_offset_m)).collect();
    ConvexSupportConstructionProof::from_tracked(volume,centroid,&vertices,&planes,position,normal)
}
