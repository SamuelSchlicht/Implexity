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
use crate::native_union_arrangement::{PhysicalSegment2,positive_labeled_union};
use crate::native_union_boundary::PhysicalFacet;
use crate::native_union_quadrature::UnionNode;
use implexity_ad::Scalar;
use implexity_core::{CaeError,CaeResult};
use std::collections::BTreeMap;

pub(crate) struct PhysicalPlaneSample<S:TransientGasScalar,L> {
    pub(crate) label:L,
    pub(crate) source_uv:[S;2],
    pub(crate) source_receipt:crate::native_union_arrangement::OutputSweepReceipt,
    pub(crate) position_m:[S;3],
    pub(crate) area_m2:S,
    pub(crate) normal:[S;3],
}
pub(crate) struct SourceEndpointIncidence<S:TransientGasScalar>{target_owner:Vec<usize>,source_owner:Vec<usize>,feature:Vec<usize>,point:[S;2]}
impl<S:TransientGasScalar> SourceEndpointIncidence<S>{pub(crate) fn target_owner(&self)->&[usize]{&self.target_owner}pub(crate) fn source_owner(&self)->&[usize]{&self.source_owner}pub(crate) fn feature(&self)->&[usize]{&self.feature}pub(crate) fn point(&self)->[S;2]{self.point}}
fn emitted_part_contains(key:&[usize],kind:usize,wanted:&[usize])->bool{if key==wanted{return true;}if key.first()!=Some(&kind){return false;}let mut at=1;while at<key.len(){let count=key[at];at+=1;let Some(end)=at.checked_add(count).filter(|&e|e<=key.len())else{return false;};if key[at..end]==*wanted||(kind==31&&emitted_part_contains(&key[at..end],31,wanted)){return true;}at=end;}false}
fn sub<S:Scalar>(a:[S;3],b:[S;3])->[S;3] {std::array::from_fn(|k|a[k]-b[k])}
fn dot<S:Scalar>(a:[S;3],b:[S;3])->S {(0..3).map(|k|a[k]*b[k]).fold(S::zero(),|a,b|a+b)}
fn cross<S:Scalar>(a:[S;3],b:[S;3])->[S;3] {[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]]}
fn same<S:TransientGasScalar>(a:S,b:S)->bool {match(a.geometry_first_order_directions(),b.geometry_first_order_directions()){(Some(x),Some(y))=>a.value().to_bits()==b.value().to_bits()&&x.iter().map(|v|v.to_bits()).eq(y.iter().map(|v|v.to_bits())),_=>false}}

fn zero<S:TransientGasScalar>(a:S)->bool {a.value()==0.&&a.geometry_first_order_directions().is_some_and(|v|v.iter().all(|x|*x==0.))}
fn feature(kind:usize,a:&[usize],b:&[usize],plane:&[usize])->Vec<usize> {let(a,b)=if a<=b{(a,b)}else{(b,a)};[vec![kind,a.len()],a.to_vec(),vec![b.len()],b.to_vec(),vec![plane.len()],plane.to_vec()].concat()}

pub(crate) fn prepare_plane_face<S:TransientGasScalar,L:Clone>(
    window:&PhysicalFacet<S>,facets:&[PhysicalFacet<S>],refinement:usize,frame:[f64;2],
    classify:impl Fn([f64;3],[f64;3])->CaeResult<Option<L>>,
)->CaeResult<Vec<PhysicalPlaneSample<S,L>>> {
    prepare_plane_face_with_plane_specs(window,facets,&[],refinement,frame,classify)
}

pub(crate) fn prepare_source_plane_face<S:TransientGasScalar,L:Clone>(
    window:&PhysicalFacet<S>,facets:&[PhysicalFacet<S>],
    planes:&[crate::native_source_face_windows::FixedGridPlane],
    refinement:usize,frame:[f64;2],classify:impl Fn([f64;3],[f64;3])->CaeResult<Option<L>>,
)->CaeResult<Vec<PhysicalPlaneSample<S,L>>> {
    let specs:Vec<_>=planes.iter().map(|p|(p.identity(),p.axis(),p.coordinate_m())).collect();
    prepare_plane_face_with_plane_specs(window,facets,&specs,refinement,frame,classify)
}

pub(crate) fn prepare_owned_source_plane_face<'g,B:crate::gas_geometry_time::GeometryTimeBase,L:Clone>(
    source:&crate::native_source_face_windows::NativeSourceFaceWindows<'g,B>,window_index:usize,candidates:&[usize],
    refinement:usize,frame:[f64;2],classify:impl Fn([f64;3],[f64;3])->CaeResult<Option<L>>,
)->CaeResult<Vec<PhysicalPlaneSample<crate::gas_geometry_time::GeometryTime<B>,L>>> {
    prepare_owned_source_plane_face_affine(source,window_index,candidates,refinement,frame,|point,normal,_uv|classify(point,normal))
}

pub(crate) fn prepare_owned_source_plane_face_affine<'g,B:crate::gas_geometry_time::GeometryTimeBase,L:Clone>(
    source:&crate::native_source_face_windows::NativeSourceFaceWindows<'g,B>,window_index:usize,candidates:&[usize],
    refinement:usize,frame:[f64;2],classify:impl Fn([f64;3],[f64;3],[f64;2])->CaeResult<Option<L>>,
)->CaeResult<Vec<PhysicalPlaneSample<crate::gas_geometry_time::GeometryTime<B>,L>>> {prepare_owned_source_plane_face_sweep_inner(source,window_index,candidates,refinement,frame,false,|p,n,uv,_|classify(p,n,uv))}
pub(crate) fn prepare_owned_source_plane_face_sweep<'g,B:crate::gas_geometry_time::GeometryTimeBase,L:Clone>(
    source:&crate::native_source_face_windows::NativeSourceFaceWindows<'g,B>,window_index:usize,candidates:&[usize],
    refinement:usize,frame:[f64;2],classify:impl Fn([f64;3],[f64;3],[f64;2],&crate::native_union_arrangement::SweepPointReceipt)->CaeResult<Option<L>>,
)->CaeResult<Vec<PhysicalPlaneSample<crate::gas_geometry_time::GeometryTime<B>,L>>> {prepare_owned_source_plane_face_sweep_inner(source,window_index,candidates,refinement,frame,true,classify)}
pub(crate) fn prepare_owned_source_plane_face_sweep_inner<'g,B:crate::gas_geometry_time::GeometryTimeBase,L:Clone>(
    source:&crate::native_source_face_windows::NativeSourceFaceWindows<'g,B>,window_index:usize,candidates:&[usize],
    refinement:usize,frame:[f64;2],owned_domain:bool,classify:impl Fn([f64;3],[f64;3],[f64;2],&crate::native_union_arrangement::SweepPointReceipt)->CaeResult<Option<L>>,
)->CaeResult<Vec<PhysicalPlaneSample<crate::gas_geometry_time::GeometryTime<B>,L>>> {
    use crate::gas_geometry_time::GeometryTime;
    let window=source.windows().get(window_index).ok_or_else(||CaeError::contract("source plane window absent"))?.facet();
    let mut facets=Vec::with_capacity(candidates.len());let mut coordinates=Vec::with_capacity(candidates.len());
    for &candidate in candidates{
        let facet=source.windows().get(candidate).ok_or_else(||CaeError::contract("source plane candidate absent"))?.facet();
        let mut row=[None;3];
        for corner in 0..3{if let Some(token)=source.native_closed_corner_uv(window_index,candidate,corner)?{
            let proof=token.certify_recomposition()?;proof.validate_owner(source,window_index,candidate,corner)?;let uv=proof.uv();let origin=window.vertices[0];let u=sub(window.vertices[1],origin);let v=sub(window.vertices[2],origin);let reconstructed:[GeometryTime<B>;3]=std::array::from_fn(|k|origin[k]+u[k]*uv[0]+v[k]*uv[1]);proof.validate_bindings(facet.vertices[corner],reconstructed,uv)?;row[corner]=Some(uv.map(GeometryTime::<B>::from_f64));
        }else if let Some(token)=source.column_uv(window_index,candidate,corner)?{
            token.validate_point(source,window_index,candidate,corner,facet.vertices[corner])?;
            let proof=token.certify_recomposition()?;
            proof.validate_owner(source,window_index,candidate,corner)?;
            let uv=proof.uv();
            let origin=window.vertices[0];let u=sub(window.vertices[1],origin);let v=sub(window.vertices[2],origin);
            let reconstructed: [GeometryTime<B>;3]=std::array::from_fn(|k|origin[k]+u[k]*uv[0]+v[k]*uv[1]);
            proof.validate_bindings(facet.vertices[corner],reconstructed,uv)?;
            row[corner]=Some(uv.map(GeometryTime::<B>::from_f64));
        }else if let Some(token)=source.parallelogram_uv(window_index,candidate,corner)?{
            let proof=token.certify_recomposition()?;
            proof.validate_owner(source,window_index,candidate,corner)?;
            let uv=proof.uv();
            let origin=window.vertices[0];let u=sub(window.vertices[1],origin);let v=sub(window.vertices[2],origin);
            let reconstructed: [GeometryTime<B>;3]=std::array::from_fn(|k|origin[k]+u[k]*uv[0]+v[k]*uv[1]);
            proof.validate_bindings(facet.vertices[corner],reconstructed,uv)?;
            row[corner]=Some(uv.map(GeometryTime::<B>::from_f64));
        }else if let Some(token)=source.native_endpoint_alias(window_index,candidate,corner)?{
            let proof=token.certify_recomposition()?;
            proof.validate_owner(source,window_index,candidate,corner)?;
            let uv=proof.uv();
            let origin=window.vertices[0];let u=sub(window.vertices[1],origin);let v=sub(window.vertices[2],origin);
            let reconstructed: [GeometryTime<B>;3]=std::array::from_fn(|k|origin[k]+u[k]*uv[0]+v[k]*uv[1]);
            proof.validate_bindings(facet.vertices[corner],reconstructed,uv)?;
            row[corner]=Some(uv.map(GeometryTime::<B>::from_f64));
        }else if let Some(token)=source.affine_corner_uv(window_index,candidate,corner)?{
            let proof=token.certify_recomposition()?;
            proof.validate_owner(source,window_index,candidate,corner)?;
            let uv=proof.uv();
            let origin=window.vertices[0];let u=sub(window.vertices[1],origin);let v=sub(window.vertices[2],origin);
            let reconstructed: [GeometryTime<B>;3]=std::array::from_fn(|k|origin[k]+u[k]*uv[0]+v[k]*uv[1]);
            proof.validate_bindings(facet.vertices[corner],reconstructed,uv)?;
            row[corner]=Some(uv.map(GeometryTime::<B>::from_f64));
        }else if let Some(token)=source.reference_fibre_uv(window_index,candidate,corner)?{
            let proof=token.certify_recomposition()?;
            proof.validate_owner(source,window_index,candidate,corner)?;
            let uv=proof.uv();
            let origin=window.vertices[0];let u=sub(window.vertices[1],origin);let v=sub(window.vertices[2],origin);
            let reconstructed: [GeometryTime<B>;3]=std::array::from_fn(|k|origin[k]+u[k]*uv[0]+v[k]*uv[1]);
            proof.validate_bindings(facet.vertices[corner],reconstructed,uv)?;
            row[corner]=Some(uv.map(GeometryTime::<B>::from_f64));
        }else if let Some(token)=source.rational_four_column_uv(window_index,candidate,corner)?{
            let proof=token.certify_recomposition()?;
            proof.validate_owner(source,window_index,candidate,corner)?;
            let uv=proof.uv();
            let origin=window.vertices[0];let u=sub(window.vertices[1],origin);let v=sub(window.vertices[2],origin);
            let reconstructed: [GeometryTime<B>;3]=std::array::from_fn(|k|origin[k]+u[k]*uv[0]+v[k]*uv[1]);
            proof.validate_bindings(facet.vertices[corner],reconstructed,uv)?;
            row[corner]=Some(uv.map(GeometryTime::<B>::from_f64));
        }else if let Some(token)=source.present_primal_corner_incidence(window_index,candidate,corner)?{
            row[corner]=Some(token.coordinates(source,window_index,candidate,corner,facet.vertices[corner])?);
        }}
        facets.push(facet.clone());coordinates.push(row);
    }
    let specs:Vec<_>=source.grid_planes().iter().map(|p|(p.identity(),p.axis(),p.coordinate_m())).collect();
    prepare_plane_face_with_coordinates_sweep(window,&facets,&specs,refinement,frame,Some(&coordinates),owned_domain,classify)
}

pub(crate) fn prepare_plane_face_with_plane_specs<S:TransientGasScalar,L:Clone>(
    window:&PhysicalFacet<S>,facets:&[PhysicalFacet<S>],planes:&[([usize;2],usize,f64)],
    refinement:usize,frame:[f64;2],classify:impl Fn([f64;3],[f64;3])->CaeResult<Option<L>>,
)->CaeResult<Vec<PhysicalPlaneSample<S,L>>> {
    prepare_plane_face_with_coordinates(window,facets,planes,refinement,frame,None,classify)
}

fn prepare_plane_face_with_coordinates<S:TransientGasScalar,L:Clone>(
    window:&PhysicalFacet<S>,facets:&[PhysicalFacet<S>],planes:&[([usize;2],usize,f64)],
    refinement:usize,frame:[f64;2],coordinates:Option<&[[Option<[S;2]>;3]]>,
    classify:impl Fn([f64;3],[f64;3])->CaeResult<Option<L>>,
)->CaeResult<Vec<PhysicalPlaneSample<S,L>>> {
    prepare_plane_face_with_coordinates_affine(window,facets,planes,refinement,frame,coordinates,|point,normal,_uv|classify(point,normal))
}

fn prepare_plane_face_with_coordinates_affine<S:TransientGasScalar,L:Clone>(
    window:&PhysicalFacet<S>,facets:&[PhysicalFacet<S>],planes:&[([usize;2],usize,f64)],
    refinement:usize,frame:[f64;2],coordinates:Option<&[[Option<[S;2]>;3]]>,
    classify:impl Fn([f64;3],[f64;3],[f64;2])->CaeResult<Option<L>>,
)->CaeResult<Vec<PhysicalPlaneSample<S,L>>> {prepare_plane_face_with_coordinates_sweep(window,facets,planes,refinement,frame,coordinates,false,|p,n,uv,_|classify(p,n,uv))}
fn prepare_plane_face_with_coordinates_sweep<S:TransientGasScalar,L:Clone>(
    window:&PhysicalFacet<S>,facets:&[PhysicalFacet<S>],planes:&[([usize;2],usize,f64)],
    refinement:usize,frame:[f64;2],coordinates:Option<&[[Option<[S;2]>;3]]>,owned_domain:bool,
    classify:impl Fn([f64;3],[f64;3],[f64;2],&crate::native_union_arrangement::SweepPointReceipt)->CaeResult<Option<L>>,
)->CaeResult<Vec<PhysicalPlaneSample<S,L>>> {
    if coordinates.is_some_and(|c|c.len()!=facets.len()){return Err(CaeError::contract("source plane coordinate shape differs"));}
    let valid=|f:&PhysicalFacet<S>|!f.owner.is_empty()&&f.vertex_features.iter().all(|p|!p.is_empty())&&f.vertices.iter().flatten().all(|v|v.finite_first_order()&&v.geometry_first_order_directions().is_some());
    if !valid(window)||facets.iter().any(|f|!valid(f)){return Err(CaeError::contract("physical plane feature ownership is absent"));}
    let origin=window.vertices[0];let u=sub(window.vertices[1],origin);let v=sub(window.vertices[2],origin);
    let raw_normal=cross(u,v);let jacobian=dot(raw_normal,raw_normal).sqrt();
    let uu=dot(u,u);let uv=dot(u,v);let vv=dot(v,v);let determinant=uu*vv-uv*uv;
    if !jacobian.value().is_finite()||jacobian.value()<=0.||!determinant.value().is_finite()||determinant.value()<=0.{return Err(CaeError::contract("physical plane loses rank"));}
    let normal=crate::native_normalization::normalize(raw_normal,jacobian,crate::native_normalization::NormalFormula::SourcePlane);
    let project=|p:[S;3]| {let d=sub(p,origin);let du=dot(d,u);let dv=dot(d,v);[(du*vv-dv*uv)/determinant,(dv*uu-du*uv)/determinant]};
    let mut segments=Vec::<PhysicalSegment2<S>>::new();let mut identities=BTreeMap::<[Vec<usize>;2],PhysicalSegment2<S>>::new();
    let mut add=|segment:PhysicalSegment2<S>,shared_corners:[bool;2]|->CaeResult<()> {
        let mut key=segment.endpoints.clone().map(|p|p.feature);key.sort();
        if let Some(previous)=identities.get(&key){
            for p in &segment.endpoints{let prior=previous.endpoints.iter().find(|q|q.feature==p.feature).ok_or_else(||CaeError::contract("physical plane incidence differs"))?;if(0..2).any(|k|!same(p.point[k],prior.point[k])){return Err(CaeError::contract("physical plane shared feature has different motion"));}}
            return Ok(());
        }
        identities.insert(key,segment.clone());
        let distance=|p:[S;2]|[p[0],p[1],S::one()-(p[0]+p[1])];let first=distance(segment.endpoints[0].point);let second=distance(segment.endpoints[1].point);
        if(0..3).any(|k|first[k].value()<0.&&second[k].value()<0.){return Ok(());}
        if coordinates.is_some()&&(0..3).any(|k|(first[k].value()<0.&&shared_corners[1]&&zero(second[k]))||(second[k].value()<0.&&shared_corners[0]&&zero(first[k]))){return Ok(());}
        segments.push(segment);Ok(())
    };
    for i in 0..3 {let j=(i+1)%3;let point=|k:usize|UnionNode{feature:feature(9,&window.vertex_features[k],&[],&window.owner),point:match k{0=>[S::zero(),S::zero()],1=>[S::one(),S::zero()],_=>[S::zero(),S::one()]}};add(PhysicalSegment2{owner:feature(10,&window.vertex_features[i],&window.vertex_features[j],&window.owner),endpoints:[point(i),point(j)]},[false;2])?;}
    let mut grid_incidence=Vec::<(UnionNode<S>,Vec<usize>,Vec<usize>)>::new();
    let mut grid_identities=BTreeMap::new();
    for &(identity,axis,coordinate) in planes {
        if axis>=3 || !coordinate.is_finite() || identity[0]!=axis {return Err(CaeError::contract("physical material face grid plane is invalid"));}
        if let Some(previous)=grid_identities.insert(identity,coordinate.to_bits()) {
            if previous!=coordinate.to_bits(){return Err(CaeError::contract("physical material face grid plane identity differs"));}
            continue;
        }
        let distances=window.vertices.map(|p|p[axis]-coordinate);
        if distances.iter().all(|d|d.value()==0.) {
            if distances.iter().any(|&d|!zero(d)){return Err(CaeError::contract("material face crosses a coincident fixed grid plane"));}
            continue;
        }
        let mut nodes=Vec::new();
        for i in 0..3 {
            if distances[i].value()==0. {
                if !zero(distances[i]){return Err(CaeError::contract("material face vertex crosses a fixed grid plane incidence"));}
                nodes.push(UnionNode{feature:feature(9,&window.vertex_features[i],&[],&window.owner),point:match i{0=>[S::zero(),S::zero()],1=>[S::one(),S::zero()],_=>[S::zero(),S::one()]}});
            }
            let j=(i+1)%3;
            if (distances[i].value()>0.&&distances[j].value()<0.)||(distances[i].value()<0.&&distances[j].value()>0.) {
                let(a,b)=if window.vertex_features[i]<=window.vertex_features[j]{(i,j)}else{(j,i)};
                let t=distances[a]/(distances[a]-distances[b]);
                let uv=|i:usize|match i{0=>[S::zero(),S::zero()],1=>[S::one(),S::zero()],_=>[S::zero(),S::one()]};
                let first=uv(a);let last=uv(b);
                let node=UnionNode{feature:feature(15,&window.vertex_features[a],&window.vertex_features[b],&identity),point:std::array::from_fn(|k|first[k]+(last[k]-first[k])*t)};
                if coordinates.is_some(){grid_incidence.push((node.clone(),feature(10,&window.vertex_features[a],&window.vertex_features[b],&window.owner),feature(16,&identity,&[],&window.owner)));}
                nodes.push(node);
            }
        }
        if nodes.len()<2{continue;}
        if nodes.len()!=2{return Err(CaeError::contract("material face fixed grid section is not a unique segment"));}
        add(PhysicalSegment2{owner:feature(16,&identity,&[],&window.owner),endpoints:[nodes.remove(0),nodes.remove(0)]},[false;2])?;
    }
    for (facet_index,facet) in facets.iter().enumerate() {
        if facet.owner==window.owner && facet.vertex_features.iter().all(|f|window.vertex_features.contains(f)) {
            for(i,f)in facet.vertex_features.iter().enumerate(){let j=window.vertex_features.iter().position(|v|v==f).ok_or_else(||CaeError::contract("physical window feature is absent"))?;if(0..3).any(|k|!same(facet.vertices[i][k],window.vertices[j][k])){return Err(CaeError::contract("physical window feature binding differs"));}}
            continue;
        }
        let shared: [Option<usize>;3]=std::array::from_fn(|i|window.vertex_features.iter().position(|f|*f==facet.vertex_features[i]));
        for(i,j)in shared.iter().enumerate(){if let Some(j)=j{if(0..3).any(|k|!same(facet.vertices[i][k],window.vertices[*j][k])){return Err(CaeError::contract("physical shared corner binding differs"));}}}
        let known=|j:usize|match j{0=>[S::zero(),S::zero()],1=>[S::one(),S::zero()],_=>[S::zero(),S::one()]};
        let authenticated=coordinates.map_or([None;3],|c|c[facet_index]);
        for i in 0..3{if let Some(point)=authenticated[i]{if point.iter().any(|v|!v.finite_first_order()||v.geometry_first_order_directions().is_none()){return Err(CaeError::contract("source plane coordinate is nonfinite"));}if let Some(j)=shared[i]{let exact=known(j);if(0..2).any(|k|!same(point[k],exact[k])){return Err(CaeError::contract("source plane coordinate disagrees with shared corner"));}}}}
        let project_corner=|i:usize|match authenticated[i]{Some(p)=>p,None=>match shared[i]{Some(j)=>known(j),None=>project(facet.vertices[i])}};
        let distances=std::array::from_fn::<_,3,_>(|i|if authenticated[i].is_some()||shared[i].is_some(){S::zero()}else{dot(raw_normal,sub(facet.vertices[i],origin))});
        if distances.iter().all(|d|d.value()==0.) {
            if distances.iter().any(|&d|!zero(d)){return Err(CaeError::contract("coplanar physical plane footprint has uncertified normal motion"));}
            for i in 0..3 {let j=(i+1)%3;add(PhysicalSegment2{owner:feature(11,&facet.vertex_features[i],&facet.vertex_features[j],&window.owner),endpoints:[i,j].map(|k|UnionNode{feature:feature(12,&facet.vertex_features[k],&[],&window.owner),point:project_corner(k)})},[shared[i].is_some(),shared[j].is_some()])?;}
            continue;
        }
        let mut nodes=Vec::new();
        for i in 0..3 {
            if distances[i].value()==0. {if !zero(distances[i]){return Err(CaeError::contract("physical vertex crosses the material plane stratum"));}nodes.push(UnionNode{feature:feature(12,&facet.vertex_features[i],&[],&window.owner),point:project_corner(i)});}
            let j=(i+1)%3;
            if (distances[i].value()>0.&&distances[j].value()<0.)||(distances[i].value()<0.&&distances[j].value()>0.) {
                let(a,b)=if facet.vertex_features[i]<=facet.vertex_features[j]{(i,j)}else{(j,i)};
                let t=distances[a]/(distances[a]-distances[b]);let p=std::array::from_fn(|k|facet.vertices[a][k]+(facet.vertices[b][k]-facet.vertices[a][k])*t);
                nodes.push(UnionNode{feature:feature(13,&facet.vertex_features[i],&facet.vertex_features[j],&window.owner),point:project(p)});
            }
        }
        if nodes.len()<2{continue;}if nodes.len()!=2{return Err(CaeError::contract("physical material plane section is not a unique segment"));}
        add(PhysicalSegment2{owner:feature(14,&facet.owner,&[],&window.owner),endpoints:[nodes.remove(0),nodes.remove(0)]},[false;2])?;
    }
    let segments=crate::native_physical_segment_overlay::interval_overlay(&segments)?;
    let mut owned_incidence=Vec::new();
    for (node,edge,grid) in grid_incidence{for target in &segments{if !emitted_part_contains(&target.owner,32,&edge){continue;}let axis=usize::from((target.endpoints[1].point[1]-target.endpoints[0].point[1]).value().abs()>(target.endpoints[1].point[0]-target.endpoints[0].point[0]).value().abs());let a=target.endpoints[0].point[axis].value();let b=target.endpoints[1].point[axis].value();if node.point[axis].value()<a.min(b)||node.point[axis].value()>a.max(b){continue;}for source in &segments{if !emitted_part_contains(&source.owner,32,&grid){continue;}for endpoint in &source.endpoints{if emitted_part_contains(&endpoint.feature,31,&node.feature){if(0..2).any(|k|!same(endpoint.point[k],node.point[k])){return Err(CaeError::contract("source grid endpoint binding differs"));}owned_incidence.push(SourceEndpointIncidence{target_owner:target.owner.clone(),source_owner:source.owner.clone(),feature:endpoint.feature.clone(),point:endpoint.point});}}}}}
    let samples=crate::native_union_arrangement::positive_labeled_union_with_source_incidence_and_sweep(&segments,&owned_incidence,refinement,frame,|p,receipt| {
        if owned_domain{for corner in 0..3{if receipt.barycentric_sign(corner).map_err(str::to_string)?<0{return Ok(None);}}}else if p[0]<0.||p[1]<0.||p[0]+p[1]>1.{return Ok(None);}
        let point=std::array::from_fn(|k|origin[k].value()+u[k].value()*p[0]+v[k].value()*p[1]);
        classify(point,normal.map(|v|v.value()),p,receipt).map_err(|e|e.to_string())
    }).map_err(CaeError::contract)?;
    Ok(samples.into_iter().map(|q|PhysicalPlaneSample{label:q.label,source_uv:q.sample.point,source_receipt:q.receipt,position_m:std::array::from_fn(|k|origin[k]+u[k]*q.sample.point[0]+v[k]*q.sample.point[1]),area_m2:q.sample.weight*jacobian,normal}).collect())
}




