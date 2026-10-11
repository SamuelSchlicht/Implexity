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

use super::{CutBoundary, CutPolygon, Plane};
use crate::gas_transient_kinetic::TransientGasScalar;
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use crate::native_geometry_construction_error::ConstructionJet;
use std::sync::Arc;
use crate::native_geometry_source::{NativeElementSource,NativeChartPointSource,NativeNodeSource};

#[derive(Clone, Debug)]
pub struct CutLocalGeometry<S: Scalar> {
    pub(crate) point_constructions: Vec<PointConstruction<S>>,
    pub(crate) anchor_m: [f64; 3],
    pub(crate) vertices_m: Vec<[S; 3]>,
    pub(crate) plane_origin_m: [S; 3],
    pub(crate) incidences: Vec<Vec<CutPlaneIncidence<S>>>,
    pub(crate) plane_incidence: Option<CutPlaneIncidence<S>>,
}
#[derive(Clone, Debug)]
pub(crate) struct ClipPointConstruction<S:Scalar> {
    point:[S;3], anchor:[f64;3], bounds:[ConstructionJet;3],
}
#[derive(Clone, Debug)]
pub(crate) enum PointConstruction<S:Scalar> {
    Leaf,
    Bound(Arc<ClipPointConstruction<S>>),
    Unproved,
}
impl<S:Scalar> PointConstruction<S> {
    fn mapped<T:Scalar>(&self)->PointConstruction<T> {
        match self {Self::Leaf=>PointConstruction::Leaf,_=>PointConstruction::Unproved}
    }
}
impl<S:TransientGasScalar> PointConstruction<S> {
    pub(crate) fn from_chart_source(source:&NativeChartPointSource<S>,point:[S;3],anchor:[f64;3])->Self {
        crate::native_geometry_construction_error::chart_point_expression_error(source,anchor,point).map_or(Self::Unproved,|bounds|Self::Bound(Arc::new(ClipPointConstruction{point,anchor,bounds})))
    }
    fn bounds(&self,point:[S;3],anchor:[f64;3])->Option<&[ConstructionJet;3]> {
        match self {Self::Bound(p) if p.anchor==anchor && point_same(p.point,point) && (0..3).all(|k|p.bounds[k].matches(point[k]))=>Some(&p.bounds),_=>None}
    }
    fn represented(&self,point:[S;3],anchor:[f64;3])->bool {
        matches!(self,Self::Leaf) || self.bounds(point,anchor).is_some()
    }
    fn encloses(&self,point:[S;3],anchor:[f64;3],target:[S;3],frame:[f64;3])->bool {
        anchor==frame && self.bounds(point,anchor).is_some_and(|p|(0..3).all(|k|p[k].encloses_scalar(target[k])))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct NativePlaneFace<S: Scalar> {
    source:Option<Arc<NativeElementSource<S>>>,
    body: usize,
    nodes: [usize; 3],
    vertices: [[S; 3]; 3],
    anchor: [f64; 3],
    normal: [S; 3],
    offset: S,
}
impl<S: Scalar> NativePlaneFace<S> {
    pub(crate) fn new(body: usize, nodes: [usize; 3], vertices: [[S; 3]; 3], normal: [S; 3], offset: S) -> Self {
        Self { source:None,body, nodes, vertices, anchor: [0.;3], normal, offset }
    }
    pub(crate) fn set_anchor(&mut self, anchor: [f64;3]) { self.anchor=anchor; }
    pub(crate) fn set_source(&mut self,source:Arc<NativeElementSource<S>>) {self.source=Some(source);}
    fn map_scalars<T: Scalar>(&self, map: impl Fn(S)->T) -> NativePlaneFace<T> {
        NativePlaneFace { source:self.source.as_ref().map(|s|Arc::new(s.map_scalars(&map))),body:self.body, nodes:self.nodes, vertices:self.vertices.map(|p|p.map(&map)), anchor:self.anchor, normal:self.normal.map(&map), offset:map(self.offset) }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct NativeLevelsetPlane<S:Scalar> {
    source:Option<Arc<NativeElementSource<S>>>,body:usize,element:usize,nodes:[usize;4],vertices:[[S;3];4],values:[S;4],iso:f64,anchor:[f64;3],normal:[S;3],offset:S,
}
impl<S:Scalar> NativeLevelsetPlane<S> {
    pub(crate) fn new(body:usize,element:usize,nodes:[usize;4],vertices:[[S;3];4],values:[S;4],iso:f64,normal:[S;3],offset:S)->Self {Self{source:None,body,element,nodes,vertices,values,iso,anchor:[0.;3],normal,offset}}
    pub(crate) fn set_anchor(&mut self,anchor:[f64;3]) {self.anchor=anchor;}
    pub(crate) fn set_source(&mut self,source:Arc<NativeElementSource<S>>) {self.source=Some(source);}
    pub(crate) fn map_scalars<T:Scalar>(&self,map:impl Fn(S)->T)->NativeLevelsetPlane<T> {NativeLevelsetPlane{source:self.source.as_ref().map(|s|Arc::new(s.map_scalars(&map))),body:self.body,element:self.element,nodes:self.nodes,vertices:self.vertices.map(|p|p.map(&map)),values:self.values.map(&map),iso:self.iso,anchor:self.anchor,normal:self.normal.map(&map),offset:map(self.offset)}}
}
impl<S:TransientGasScalar> NativeLevelsetPlane<S> {
    fn valid(&self,p:&Plane<S>)->bool {
        let finite=|v:S|v.finite_first_order() && v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite()));
        if p.native_face.is_some() || p.subdomain_face.is_some() || !matches!(p.boundary,CutBoundary::Native{body,element,face:None,physical:true} if body==self.body && element==self.element) || !self.iso.is_finite() || self.anchor.iter().any(|v|!v.is_finite()) || (0..4).any(|i|self.nodes[i+1..].contains(&self.nodes[i])) || self.vertices.iter().flatten().copied().chain(self.values).chain(self.normal).chain([self.offset]).any(|v|!finite(v)){return false;}
        let source_coefficients=if let Some(source)=&self.source {
            if source.validate().is_err() || source.body!=self.body || source.element!=self.element || source.nodes!=self.nodes {return false;}
            let Ok(points)=source.vertices_in_frame(self.anchor) else{return false;};
            if !(0..4).all(|i|point_same(self.vertices[i],points[i])) {return false;}
            let Ok(coefficients)=source.levelset(self.values,self.iso) else{return false;};
            Some(coefficients)
        } else {None};
        let (normal,offset)=if let Some(coefficients)=source_coefficients {coefficients} else {
            if !matches!(crate::native_exact_affine_predicate::exact_framed_tetra_sign(self.vertices.map(|p|p.map(|v|v.value())),[self.anchor;4]),Ok(-1)|Ok(1)){return false;}
            let e=[super::sub(self.vertices[1],self.vertices[0]),super::sub(self.vertices[2],self.vertices[0]),super::sub(self.vertices[3],self.vertices[0])];
            let c=[super::cross(e[1],e[2]),super::cross(e[2],e[0]),super::cross(e[0],e[1])];let determinant=super::dot(e[0],c[0]);
            if determinant.value()==0. || !finite(determinant){return false;}
            let gradient:[S;3]=std::array::from_fn(|k|((self.values[1]-self.values[0])*c[0][k]+(self.values[2]-self.values[0])*c[1][k]+(self.values[3]-self.values[0])*c[2][k])/determinant);
            let length=super::dot(gradient,gradient).sqrt();if length.value()<=0. || !finite(length){return false;}
            (gradient.map(|v|v/length),(self.values[0]-self.iso)/length)
        };
        if !(0..3).all(|k|same(self.normal[k],normal[k])) || !same(self.offset,offset){return false;}
        let orientation=if (0..3).all(|k|same(self.normal[k],p.normal[k])){1.}else if (0..3).all(|k|same(self.normal[k],-p.normal[k])){-1.}else{return false;};
        let (anchor,origin)=p.local_origin.unwrap_or(([0.;3],p.origin));
        let origin_bound=self.source.as_ref().and_then(|source|source.points[0].point_in_frame(anchor).ok()).is_some_and(|point|point_same(origin,point))
            || maintained_plane_origins(self.normal,origin,anchor,self.vertices[0],self.anchor);
        same(p.offset,self.offset*orientation) && origin_bound
    }
    fn same_controls(&self,other:&Self)->bool {source_same(self.source.as_ref(),other.source.as_ref()) && self.body==other.body && self.element==other.element && self.nodes==other.nodes && self.iso==other.iso && (0..4).all(|i|same(self.values[i],other.values[i]) && (self.source.is_some() || framed_point_same(self.vertices[i],self.anchor,other.vertices[i],other.anchor)))}
    fn same_owner_bits(&self,other:&Self)->bool {source_same(self.source.as_ref(),other.source.as_ref()) && self.body==other.body && self.element==other.element && self.nodes==other.nodes && self.iso.to_bits()==other.iso.to_bits() && anchor_owner_bits(self.anchor,other.anchor) && (0..4).all(|i|point_owner_bits(self.vertices[i],other.vertices[i]) && scalar_owner_bits(self.values[i],other.values[i])) && point_owner_bits(self.normal,other.normal) && scalar_owner_bits(self.offset,other.offset)}
}

#[derive(Clone,Debug)]
pub(crate) enum SubdomainNativeSupport<S:Scalar> {Face(NativePlaneFace<S>),Levelset(NativeLevelsetPlane<S>)}
impl<S:Scalar> SubdomainNativeSupport<S> {
    pub(crate) fn map_scalars<T:Scalar>(&self,map:impl Fn(S)->T)->SubdomainNativeSupport<T>{match self {Self::Face(r)=>SubdomainNativeSupport::Face(r.map_scalars(map)),Self::Levelset(r)=>SubdomainNativeSupport::Levelset(r.map_scalars(map))}}
}
impl<S:TransientGasScalar> SubdomainNativeSupport<S> {
    pub(crate) fn from_plane(p:&Plane<S>)->Option<Self>{
        if let Some(r)=&p.native_face {if r.source.is_some() && r.valid(p){return Some(Self::Face(r.clone()));}}
        if let Some(r)=&p.native_levelset {if r.source.is_some() && r.valid(p){return Some(Self::Levelset(r.clone()));}}
        None
    }
    pub(crate) fn plane(&self)->CaeResult<Plane<S>>{
        let (normal,offset,anchor,origin,boundary,native_face,native_levelset)=match self {
            Self::Face(r)=>{let source=r.source.as_ref().ok_or_else(||CaeError::contract("subdomain native source is absent"))?;let indices=source.face_indices(r.nodes).ok_or_else(||CaeError::contract("subdomain native face nodes differ"))?;(r.normal,r.offset,r.anchor,source.points[indices[0]].point_in_frame(r.anchor)?,CutBoundary::Native{body:r.body,element:source.element,face:Some(r.nodes),physical:false},Some(r.clone()),None)},
            Self::Levelset(r)=>{let source=r.source.as_ref().ok_or_else(||CaeError::contract("subdomain levelset source is absent"))?;(r.normal,r.offset,r.anchor,source.points[0].point_in_frame(r.anchor)?,CutBoundary::Native{body:r.body,element:r.element,face:None,physical:true},None,Some(r.clone()))},
        };
        let p=Plane{normal,offset,origin:std::array::from_fn(|k|origin[k]+anchor[k]),local_origin:Some((anchor,origin)),boundary,native_face,native_levelset,subdomain_face:None};
        if !p.native_face.as_ref().map_or_else(||p.native_levelset.as_ref().is_some_and(|r|r.valid(&p)),|r|r.valid(&p)){return Err(CaeError::contract("subdomain original support is invalid"));}Ok(p)
    }
    pub(crate) fn owns_chart_point(&self,p:&NativeChartPointSource<S>)->bool {
        if self.plane().is_err(){return false;}
        let Some(feature)=p.boundary_feature() else{return false;};let Ok(weights)=feature.weights() else{return false;};
        if feature.scales.iter().flatten().any(|pair|pair[0].to_bits()!=pair[1].to_bits()){return false;}
        if let crate::moving_contact::surface_map::SurfaceFeature::FixedTetrahedron{weights,..}=&feature.feature {if crate::native_exact_affine_predicate::exact_framed_plane_sign([1.;3],[weights[0],weights[1],weights[2]],[0.;3],[1.,-weights[3],0.],[0.;3],0.)!=Ok(0){return false;}}
        match self {
            Self::Face(r)=>{let source=r.source.as_ref().unwrap();weights.iter().all(|(id,w)|{
                if same(*w,S::zero()){return true;}
                feature.points.iter().find(|p|p.node==*id).is_some_and(|p|r.nodes.contains(id) && source.nodes.iter().position(|n|n==id).is_some_and(|i|p.same(&source.points[i])))
            })},
            Self::Levelset(r)=>{let source=r.source.as_ref().unwrap();if !feature.points.iter().enumerate().all(|(i,p)|source.nodes.iter().position(|id|*id==p.node).is_some_and(|j|p.same(&source.points[j]) && same(feature.values[i],r.values[j]))){return false;}
                match &feature.feature {
                    crate::moving_contact::surface_map::SurfaceFeature::DensityEdge{iso,..}=>iso.to_bits()==r.iso.to_bits(),
                    crate::moving_contact::surface_map::SurfaceFeature::Vertex(id)=>r.nodes.iter().position(|n|n==id).is_some_and(|i|same(r.values[i],S::from_f64(r.iso))),
                    crate::moving_contact::surface_map::SurfaceFeature::FixedTetrahedron{..}=>false,
                }
            },
        }
    }
    fn same(&self,other:&Self)->bool {match(self,other){(Self::Face(a),Self::Face(b))=>a.same_owner_bits(b),(Self::Levelset(a),Self::Levelset(b))=>a.same_owner_bits(b),_=>false}}
    fn same_primitive(&self,other:&Self)->bool {match(self,other){(Self::Face(a),Self::Face(b))=>a.body==b.body && a.nodes==b.nodes && source_same(a.source.as_ref(),b.source.as_ref()) && point_same(a.normal,b.normal) && same(a.offset,b.offset),(Self::Levelset(a),Self::Levelset(b))=>a.same_controls(b) && point_same(a.normal,b.normal) && same(a.offset,b.offset),_=>false}}
}

#[derive(Clone, Debug)]
pub(crate) struct SubdomainPlaneFace<S:Scalar> {
    chart_sources:Option<[NativeChartPointSource<S>;3]>,native_support:Option<SubdomainNativeSupport<S>>,region:usize, face:usize, vertex_ids:Option<[usize;3]>, vertices:[[S;3];3], anchor:[f64;3], normal:[S;3], offset:S,
}
impl<S:Scalar> SubdomainPlaneFace<S> {
    pub(crate) fn new(region:usize,face:usize,vertex_ids:Option<[usize;3]>,vertices:[[S;3];3],normal:[S;3],offset:S)->Self {Self{chart_sources:None,native_support:None,region,face,vertex_ids,vertices,anchor:[0.;3],normal,offset}}
    pub(crate) fn set_anchor(&mut self,anchor:[f64;3]) {self.anchor=anchor;}
    pub(crate) fn set_chart_sources(&mut self,sources:[NativeChartPointSource<S>;3]){self.chart_sources=Some(sources);}
    pub(crate) fn set_native_support(&mut self,support:SubdomainNativeSupport<S>){self.native_support=Some(support);}
    fn map_scalars<T:Scalar>(&self,map:impl Fn(S)->T)->SubdomainPlaneFace<T> {SubdomainPlaneFace{chart_sources:self.chart_sources.as_ref().map(|sources|sources.clone().map(|p|p.map_scalars(&map))),native_support:self.native_support.as_ref().map(|p|p.map_scalars(&map)),region:self.region,face:self.face,vertex_ids:self.vertex_ids,vertices:self.vertices.map(|p|p.map(&map)),anchor:self.anchor,normal:self.normal.map(&map),offset:map(self.offset)}}
}
impl<S:TransientGasScalar> SubdomainPlaneFace<S> {
    fn same_primal_source(&self,other:&Self)->bool {
        let (Some(a),Some(b))=(&self.chart_sources,&other.chart_sources) else{return false;};
        (0..3).all(|i|a[i].map_scalars(|v|v.value()).same(&b[i].map_scalars(|v|v.value()))) && match(&self.native_support,&other.native_support){(Some(a),Some(b))=>a.map_scalars(|v|v.value()).same_primitive(&b.map_scalars(|v|v.value())),(None,None)=>true,_=>false}
    }
    pub(crate) fn valid(&self,p:&Plane<S>)->bool {
        let finite=|v:S|v.finite_first_order() && v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite()));
        if p.native_face.is_some() || p.native_levelset.is_some() || !matches!(p.boundary,CutBoundary::Subdomain{region,face} if region==self.region && face==self.face) || self.face>=4 || self.anchor.iter().any(|v|!v.is_finite()) || self.vertices.iter().flatten().copied().chain(self.normal).chain([self.offset]).any(|v|!finite(v)) || !self.normal.iter().any(|v|v.value()!=0.) {return false;}
        if self.vertex_ids.is_some_and(|ids|(0..3).any(|i|ids[i+1..].contains(&ids[i]))){return false;}
        if let Some(sources)=&self.chart_sources {if !(0..3).all(|i|sources[i].point_in_frame(self.anchor).is_ok_and(|p|point_same(p,self.vertices[i]))){return false;}}
        if let Some(support)=&self.native_support {
            let Some(sources)=&self.chart_sources else{return false;};
            if !sources.iter().all(|p|support.owns_chart_point(p)){return false;}
            let Ok(original)=support.plane() else{return false;};
            let orientation=if (0..3).all(|k|same(self.normal[k],original.normal[k])){1.}else if (0..3).all(|k|same(self.normal[k],-original.normal[k])){-1.}else{return false;};
            if !same(self.offset,original.offset*orientation){return false;}
            let actual=if (0..3).all(|k|same(p.normal[k],self.normal[k])){1.}else if (0..3).all(|k|same(p.normal[k],-self.normal[k])){-1.}else{return false;};
            let (anchor,origin)=p.local_origin.unwrap_or(([0.;3],p.origin));
            return same(p.offset,self.offset*actual) && point_same(origin,plane_origin_in_frame(&original,anchor));
        }
        if !same(self.offset,S::zero()){return false;}
        let primitive=super::cross(super::sub(self.vertices[1],self.vertices[0]),super::sub(self.vertices[2],self.vertices[0]));
        let length=super::dot(primitive,primitive).sqrt();
        if length.value()<=0. || !finite(length){return false;}
        let derived=primitive.map(|v|v/length);
        if !(0..3).all(|k|same(self.normal[k],derived[k])) && !(0..3).all(|k|same(self.normal[k],-derived[k])){return false;}

        let orientation=if (0..3).all(|k|same(self.normal[k],p.normal[k])) {1.} else if (0..3).all(|k|same(self.normal[k],-p.normal[k])) {-1.} else{return false;};
        let (anchor,origin)=p.local_origin.unwrap_or(([0.;3],p.origin));
        same(p.offset,self.offset*orientation) && maintained_plane_origins(self.normal,origin,anchor,self.vertices[0],self.anchor)
    }
    fn same_owner_bits(&self,other:&Self)->bool {self.chart_sources.as_ref().zip(other.chart_sources.as_ref()).map_or(self.chart_sources.is_none() && other.chart_sources.is_none(),|(a,b)|(0..3).all(|i|a[i].same(&b[i]))) && self.native_support.as_ref().zip(other.native_support.as_ref()).map_or(self.native_support.is_none() && other.native_support.is_none(),|(a,b)|a.same(b)) && self.region==other.region && self.face==other.face && self.vertex_ids==other.vertex_ids && anchor_owner_bits(self.anchor,other.anchor) && (0..3).all(|i|point_owner_bits(self.vertices[i],other.vertices[i])) && point_owner_bits(self.normal,other.normal) && scalar_owner_bits(self.offset,other.offset)}
}
pub(crate) fn framed_point_same<S: TransientGasScalar>(a:[S;3], aa:[f64;3], b:[S;3], ba:[f64;3])->bool {
    let safe=|v:f64|v==0. || (v.abs()>=2f64.powi(-400) && v.abs()<=2f64.powi(400));
    if aa==ba && aa.iter().copied().chain(a.map(|v|v.value())).chain(b.map(|v|v.value())).all(safe) {
        return (0..3).all(|k|a[k].finite_first_order() && b[k].finite_first_order() && a[k].value()==b[k].value()
            && a[k].geometry_first_order_directions().zip(b[k].geometry_first_order_directions()).is_some_and(|(x,y)|x.iter().chain(y.iter()).all(|v|v.is_finite()) && x==y));
    }
    (0..3).all(|k| {
        if !a[k].finite_first_order() || !b[k].finite_first_order() {return false;}
        let mut axis=[0.;3]; axis[k]=1.;
        crate::native_exact_affine_predicate::exact_framed_plane_sign(axis,a.map(|v|v.value()),aa,b.map(|v|v.value()),ba,0.)==Ok(0)
        && a[k].geometry_first_order_directions().zip(b[k].geometry_first_order_directions()).is_some_and(|(x,y)|x.iter().chain(y.iter()).all(|v|v.is_finite()) && x==y)
    })
}
fn maintained_plane_origins<S: TransientGasScalar>(normal:[S;3],qo:[S;3],qa:[f64;3],po:[S;3],pa:[f64;3])->bool {
    if crate::native_exact_affine_predicate::exact_framed_plane_sign(normal.map(|v|v.value()),qo.map(|v|v.value()),qa,po.map(|v|v.value()),pa,0.)!=Ok(0){return false;}
        if framed_point_same(qo,qa,po,pa) { return true; }
        if normal.iter().any(|n|!n.finite_first_order() || !n.geometry_first_order_directions().is_some_and(|d|d.iter().all(|v|*v==0.))) { return false; }
        let qd=qo.map(|v|v.geometry_first_order_directions());
        let pd=po.map(|v|v.geometry_first_order_directions());
        if qo.iter().chain(po.iter()).any(|v|!v.finite_first_order()) { return false; }
        let (Some(q0),Some(q1),Some(q2),Some(p0),Some(p1),Some(p2))=(qd[0].as_ref(),qd[1].as_ref(),qd[2].as_ref(),pd[0].as_ref(),pd[1].as_ref(),pd[2].as_ref()) else { return false; };
        let count=q0.len();
        if [q1,q2,p0,p1,p2].iter().any(|d|d.len()!=count){return false;}
        (0..count).all(|i|crate::native_exact_affine_predicate::exact_framed_plane_sign(normal.map(|v|v.value()),[q0[i],q1[i],q2[i]],[0.;3],[p0[i],p1[i],p2[i]],[0.;3],0.)==Ok(0))
}
fn source_same<S:TransientGasScalar>(a:Option<&Arc<NativeElementSource<S>>>,b:Option<&Arc<NativeElementSource<S>>>)->bool {
    match (a,b) {(Some(a),Some(b))=>a.same(b),(None,None)=>true,_=>false}
}
fn source_primal_same<S:TransientGasScalar>(a:Option<&Arc<NativeElementSource<S>>>,b:Option<&Arc<NativeElementSource<S>>>)->bool {
    match (a,b) {(Some(a),Some(b))=>a.map_scalars(|v|v.value()).same(&b.map_scalars(|v|v.value())),_=>false}
}
impl<S: TransientGasScalar> NativePlaneFace<S> {
    fn valid(&self, p:&Plane<S>)->bool {
        if !self.normal.iter().any(|v|v.value()!=0.) || self.normal.iter().any(|v|!v.finite_first_order() || !v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|v|v.is_finite()))) {return false;}
        if let Some(source)=&self.source {
            if source.validate().is_err() || source.body!=self.body || !matches!(p.boundary,CutBoundary::Native{element,..} if element==source.element) || !same(self.offset,S::zero()) {return false;}
            let Some(indices)=source.face_indices(self.nodes) else{return false;};
            let Ok(points)=source.vertices_in_frame(self.anchor) else{return false;};
            if !(0..3).all(|i|point_same(self.vertices[i],points[indices[i]])){return false;}
            let Ok(normal)=source.face_normal(indices) else{return false;};
            if !(0..3).all(|k|same(self.normal[k],normal[k])) && !(0..3).all(|k|same(self.normal[k],-normal[k])) {return false;}
        }
        if p.subdomain_face.is_some() || p.native_levelset.is_some(){return false;}
        let CutBoundary::Native {body, face:Some(face), ..}=p.boundary else {return false;};
        let (anchor,origin)=p.local_origin.unwrap_or(([0.;3],p.origin));
        let orientation=if (0..3).all(|k|same(self.normal[k],p.normal[k])) {1.} else if (0..3).all(|k|same(self.normal[k],-p.normal[k])) {-1.} else {return false;};
        let origin_bound=self.source.as_ref().and_then(|source|source.face_indices(self.nodes).and_then(|indices|source.points[indices[0]].point_in_frame(anchor).ok())).is_some_and(|point|point_same(origin,point))
            || maintained_plane_origins(self.normal,origin,anchor,self.vertices[0],self.anchor);
        body==self.body && face==self.nodes && same(p.offset,self.offset*orientation) && origin_bound
    }
    fn agrees(&self,p:&Plane<S>,other:&Self,q:&Plane<S>)->bool {
        if self.body!=other.body{return false;}
        let shared=self.nodes.iter().enumerate().filter_map(|(i,n)|other.nodes.iter().position(|m|m==n).map(|j|(i,j))).collect::<Vec<_>>();
        if shared.is_empty(){return false;}
        let opposite=(0..3).all(|k|same(p.normal[k],-q.normal[k]));
        if !(opposite||(0..3).all(|k|same(p.normal[k],q.normal[k])))||!same(p.offset,if opposite{-q.offset}else{q.offset}){return false;}
        if !self.valid(p)||!other.valid(q){return false;}
        let shared_agrees=match (&self.source,&other.source) {
            (Some(a),Some(b))=>shared.iter().all(|&(i,j)|a.nodes.iter().position(|n|*n==self.nodes[i]).zip(b.nodes.iter().position(|n|*n==other.nodes[j])).is_some_and(|(u,v)|a.points[u].same(&b.points[v]))),
            _=>shared.iter().all(|&(i,j)|framed_point_same(self.vertices[i],self.anchor,other.vertices[j],other.anchor)),
        };
        if !shared_agrees{return false;}
        if self.source.is_some() && other.source.is_some(){return true;}
        let (pa,po)=p.local_origin.unwrap_or(([0.;3],p.origin));
        let (qa,qo)=q.local_origin.unwrap_or(([0.;3],q.origin));
        maintained_plane_origins(p.normal,qo,qa,po,pa)
    }
}
#[derive(Clone, Debug)]
pub(crate) struct CutPlaneIncidence<S: Scalar> {
    boundary: CutBoundary,
    normal: [S; 3],
    origin: [S; 3],
    offset: S,
    local_origin: Option<([f64; 3], [S; 3])>,
    native_face: Option<NativePlaneFace<S>>,
    native_levelset:Option<NativeLevelsetPlane<S>>,
    subdomain_face: Option<SubdomainPlaneFace<S>>,
}
impl<S: Scalar> CutPlaneIncidence<S> {
    fn map_scalars<T: Scalar>(&self,map:impl Fn(S)->T)->CutPlaneIncidence<T>{
        CutPlaneIncidence { native_levelset:self.native_levelset.as_ref().map(|r|r.map_scalars(&map)),boundary:self.boundary.clone(),normal:self.normal.map(&map),origin:self.origin.map(&map),offset:map(self.offset),local_origin:self.local_origin.map(|(anchor,origin)|(anchor,origin.map(&map))),native_face:self.native_face.as_ref().map(|r|r.map_scalars(&map)),subdomain_face:self.subdomain_face.as_ref().map(|r|r.map_scalars(&map)) }
    }
}
impl<S: Scalar> CutLocalGeometry<S> {
    pub(crate) fn invalidate_point_constructions(&mut self) {self.point_constructions=vec![PointConstruction::Unproved;self.vertices_m.len()];}
    pub fn anchor(&self) -> [f64; 3] {
        self.anchor_m
    }
    pub fn vertices(&self) -> &[[S; 3]] {
        &self.vertices_m
    }
    pub fn plane_origin(&self) -> [S; 3] {
        self.plane_origin_m
    }
    pub fn map_scalars<T: Scalar>(&self, map: impl Fn(S) -> T) -> CutLocalGeometry<T> {
        CutLocalGeometry {
            point_constructions:self.point_constructions.iter().map(PointConstruction::mapped).collect(),
            plane_incidence:self.plane_incidence.as_ref().map(|p|p.map_scalars(&map)),
            anchor_m: self.anchor_m,
            vertices_m: self
                .vertices_m
                .iter()
                .copied()
                .map(|p| p.map(&map))
                .collect(),
            plane_origin_m: self.plane_origin_m.map(&map),
            incidences: self
                .incidences
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|p| CutPlaneIncidence {
                            native_levelset:p.native_levelset.as_ref().map(|r|r.map_scalars(&map)),
                            native_face: p.native_face.as_ref().map(|r|r.map_scalars(&map)),
                            subdomain_face: p.subdomain_face.as_ref().map(|r|r.map_scalars(&map)),
                            boundary: p.boundary.clone(),
                            normal: p.normal.map(&map),
                            origin: p.origin.map(&map),
                            offset: map(p.offset),
                            local_origin: p
                                .local_origin
                                .map(|(anchor, origin)| (anchor, origin.map(&map))),
                        })
                        .collect()
                })
                .collect(),
        }
    }
}
fn scalar_owner_bits<S:TransientGasScalar>(a:S,b:S)->bool {
    a.value().to_bits()==b.value().to_bits()
        && a.geometry_first_order_directions().zip(b.geometry_first_order_directions()).is_some_and(|(x,y)|x.len()==y.len() && x.iter().zip(y.iter()).all(|(x,y)|x.to_bits()==y.to_bits()))
}
fn point_owner_bits<S:TransientGasScalar>(a:[S;3],b:[S;3])->bool {(0..3).all(|k|scalar_owner_bits(a[k],b[k]))}
fn anchor_owner_bits(a:[f64;3],b:[f64;3])->bool {(0..3).all(|k|a[k].to_bits()==b[k].to_bits())}
impl<S:TransientGasScalar> NativePlaneFace<S> {
    fn same_owner_bits(&self,other:&Self)->bool {
        source_same(self.source.as_ref(),other.source.as_ref()) && self.body==other.body && self.nodes==other.nodes && anchor_owner_bits(self.anchor,other.anchor)
            && (0..3).all(|i|point_owner_bits(self.vertices[i],other.vertices[i]))
            && point_owner_bits(self.normal,other.normal) && scalar_owner_bits(self.offset,other.offset)
    }
}
impl<S:TransientGasScalar> CutPlaneIncidence<S> {
    fn same_owner_bits(&self,other:&Self)->bool {
        self.boundary==other.boundary && point_owner_bits(self.normal,other.normal)
            && point_owner_bits(self.origin,other.origin) && scalar_owner_bits(self.offset,other.offset)
            && match (&self.local_origin,&other.local_origin) { (None,None)=>true,(Some((a,p)),Some((b,q)))=>anchor_owner_bits(*a,*b)&&point_owner_bits(*p,*q),_=>false }
            && match (&self.native_face,&other.native_face) {(None,None)=>true,(Some(a),Some(b))=>a.same_owner_bits(b),_=>false}
            && match (&self.native_levelset,&other.native_levelset) {(None,None)=>true,(Some(a),Some(b))=>a.same_owner_bits(b),_=>false}
            && match (&self.subdomain_face,&other.subdomain_face) {(None,None)=>true,(Some(a),Some(b))=>a.same_owner_bits(b),_=>false}
    }
}
impl<S:TransientGasScalar> CutLocalGeometry<S> {
    pub(crate) fn same_owner_bits(&self,other:&Self)->bool {
        anchor_owner_bits(self.anchor_m,other.anchor_m) && point_owner_bits(self.plane_origin_m,other.plane_origin_m)
            && self.vertices_m.len()==other.vertices_m.len() && self.vertices_m.iter().zip(other.vertices_m.iter()).all(|(a,b)|point_owner_bits(*a,*b))
            && self.incidences.len()==other.incidences.len() && self.incidences.iter().zip(other.incidences.iter()).all(|(a,b)|a.len()==b.len()&&a.iter().zip(b.iter()).all(|(a,b)|a.same_owner_bits(b)))
            && match (&self.plane_incidence,&other.plane_incidence) {(None,None)=>true,(Some(a),Some(b))=>a.same_owner_bits(b),_=>false}
    }
}
fn same<S: TransientGasScalar>(a: S, b: S) -> bool {
    match (
        a.geometry_first_order_directions(),
        b.geometry_first_order_directions(),
    ) {
        (Some(x), Some(y)) => a.finite_first_order() && b.finite_first_order() && x.iter().chain(y.iter()).all(|v|v.is_finite()) && a.value() == b.value() && x == y,
        _ => false,
    }
}
fn boundary_same(a: &CutBoundary, b: &CutBoundary) -> bool {
    match (a, b) {
        (
            CutBoundary::Native {
                body: a,
                face: Some(af),
                ..
            },
            CutBoundary::Native {
                body: b,
                face: Some(bf),
                ..
            },
        ) => {
            let (mut af, mut bf) = (*af, *bf);
            af.sort();
            bf.sort();
            a == b && af == bf
        }
        _ => a == b,
    }
}
impl<S: TransientGasScalar> CutPlaneIncidence<S> {
    pub(crate) fn boundary(&self)->&CutBoundary { &self.boundary }
    pub(crate) fn rebind_owned_plane(&self, live: &Plane<S>) -> CaeResult<Self> {
        let refuse = || CaeError::contract("cut incidence live producer differs from retained owner");
        if self.boundary != live.boundary { return Err(refuse()); }
        let finite = |v:S| v.finite_first_order() && v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite()));
        if !live.normal.iter().any(|v|v.value()!=0.) || !live.normal.iter().chain(live.origin.iter()).copied().chain([live.offset]).all(finite) {return Err(refuse());}
        let orientation = if (0..3).all(|k|self.normal[k].value()==live.normal[k].value()) {1.}
            else if (0..3).all(|k|self.normal[k].value()==-live.normal[k].value()) {-1.}
            else {return Err(refuse());};
        if self.offset.value()!=orientation*live.offset.value() {return Err(refuse());}
        let (old_anchor,old_origin)=self.local_origin.unwrap_or(([0.;3],self.origin));
        let (new_anchor,new_origin)=live.local_origin.unwrap_or(([0.;3],live.origin));
        let source_primal_bound=self.native_face.as_ref().zip(live.native_face.as_ref()).is_some_and(|(a,b)|source_primal_same(a.source.as_ref(),b.source.as_ref()))
            || self.native_levelset.as_ref().zip(live.native_levelset.as_ref()).is_some_and(|(a,b)|source_primal_same(a.source.as_ref(),b.source.as_ref()))
            || self.subdomain_face.as_ref().zip(live.subdomain_face.as_ref()).is_some_and(|(a,b)|a.same_primal_source(b));
        if !old_anchor.iter().chain(new_anchor.iter()).all(|x|x.is_finite()) || !new_origin.iter().copied().all(finite)
            || (!source_primal_bound && crate::native_exact_affine_predicate::exact_framed_plane_sign(self.normal.map(|v|v.value()),new_origin.map(|v|v.value()),new_anchor,old_origin.map(|v|v.value()),old_anchor,0.)!=Ok(0)) {return Err(refuse());}
        match &self.boundary {
            CutBoundary::Native {face:Some(_),..} => {
                let (Some(old),Some(new))=(&self.native_face,&live.native_face) else {return Err(refuse());};
                let old_plane=Plane { native_levelset:self.native_levelset.clone(), subdomain_face:self.subdomain_face.clone(),normal:self.normal,origin:self.origin,offset:self.offset,boundary:self.boundary.clone(),local_origin:self.local_origin,native_face:self.native_face.clone()};
                if old.body!=new.body || old.nodes!=new.nodes || !old.valid(&old_plane) || !new.valid(live)
                    || !(source_primal_same(old.source.as_ref(),new.source.as_ref()) || (old.source.is_none() && new.source.is_none() && (0..3).all(|i|framed_point_same(old.vertices[i].map(|v|v.value()),old.anchor,new.vertices[i].map(|v|v.value()),new.anchor)))) {return Err(refuse());}
            }
            CutBoundary::Native{face:None,..}=>{
                if self.native_levelset.is_some() || live.native_levelset.is_some(){
                let (Some(old),Some(new))=(&self.native_levelset,&live.native_levelset) else{return Err(refuse());};
                let old_plane=incidence_plane(self);
                if !old.valid(&old_plane) || !new.valid(live) || old.body!=new.body || old.element!=new.element || old.nodes!=new.nodes || old.iso!=new.iso || !(0..4).all(|i|old.values[i].value()==new.values[i].value()) || !(source_primal_same(old.source.as_ref(),new.source.as_ref()) || (old.source.is_none() && new.source.is_none() && (0..4).all(|i|framed_point_same(old.vertices[i].map(|v|v.value()),old.anchor,new.vertices[i].map(|v|v.value()),new.anchor)))){return Err(refuse());}
                }
            }
            CutBoundary::Subdomain {..} => {
                let (Some(old),Some(new))=(&self.subdomain_face,&live.subdomain_face) else{return Err(refuse());};
                let old_plane=incidence_plane(self);
                if old.region!=new.region || old.face!=new.face || old.vertex_ids!=new.vertex_ids || !old.valid(&old_plane) || !new.valid(live)
                    || !(old.same_primal_source(new) || (old.chart_sources.is_none() && new.chart_sources.is_none() && (0..3).all(|i|framed_point_same(old.vertices[i].map(|v|v.value()),old.anchor,new.vertices[i].map(|v|v.value()),new.anchor)))){return Err(refuse());}
            }
            CutBoundary::Cell {axis,..} => {
                if *axis>=3 || !(0..3).all(|k|same(live.normal[k],if k==*axis {if live.normal[k].value()>0. {S::one()} else {-S::one()}} else {S::zero()}))
                    || live.normal[*axis].value().abs()!=1. || !same(live.offset,S::zero())
                    || !new_origin[*axis].geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|*x==0.)) {return Err(refuse());}
            }
            _ => {}
        }
        let mut oriented=live.clone();
        if orientation<0. {oriented.normal=oriented.normal.map(|v|-v);oriented.offset=-oriented.offset;}
        Ok(Self::from_plane(&oriented))
    }
    pub(crate) fn from_plane(plane: &Plane<S>) -> Self {
        Self {
            native_levelset:plane.native_levelset.clone(),
            native_face: plane.native_face.clone(),
            subdomain_face: plane.subdomain_face.clone(),
            boundary: plane.boundary.clone(),
            normal: plane.normal,
            origin: plane.origin,
            offset: plane.offset,
            local_origin: plane.local_origin,
        }
    }
    pub(crate) fn matches(&self, plane: &Plane<S>) -> bool {
        if self.subdomain_face.as_ref().is_some_and(|r|r.native_support.is_some())
            && (plane.native_face.is_some() || plane.native_levelset.is_some())
            && subdomain_native_plane_equivalence(self,plane){return true;}
        if plane.subdomain_face.as_ref().is_some_and(|r|r.native_support.is_some())
            && (self.native_face.is_some() || self.native_levelset.is_some())
            && subdomain_native_plane_equivalence(&CutPlaneIncidence::from_plane(plane),&incidence_plane(self)){return true;}
        if let (Some(a),Some(b))=(&self.native_face,&plane.native_face) {
            let owned=Plane { native_levelset:self.native_levelset.clone(), subdomain_face:self.subdomain_face.clone(), native_face:self.native_face.clone(), boundary:self.boundary.clone(), normal:self.normal, origin:self.origin, offset:self.offset, local_origin:self.local_origin };
            if a.agrees(&owned,b,plane){return true;}
            if a.source.is_some() || b.source.is_some(){return false;}
        }
        if !boundary_same(&self.boundary, &plane.boundary) {
            return false;
        }
        if self.native_levelset.is_some() || plane.native_levelset.is_some(){
            let (Some(a),Some(b))=(&self.native_levelset,&plane.native_levelset) else{return false;};
            if !a.valid(&incidence_plane(self)) || !b.valid(plane) || !a.same_controls(b){return false;}
            if a.source.is_some() && b.source.is_some(){
                let orientation=if (0..3).all(|k|same(self.normal[k],plane.normal[k])){1.}else if (0..3).all(|k|same(self.normal[k],-plane.normal[k])){-1.}else{return false;};
                return same(self.offset,plane.offset*orientation);
            }
        }
        if let Some(receipt)=&self.subdomain_face {
            if !receipt.valid(plane){return false;}
            if let Some(other)=&plane.subdomain_face {
                if receipt.region!=other.region || receipt.face!=other.face || receipt.vertex_ids!=other.vertex_ids || !other.valid(plane)
                    || !match (&receipt.chart_sources,&other.chart_sources){(Some(a),Some(b))=>(0..3).all(|i|a[i].same(&b[i])),(None,None)=>(0..3).all(|i|framed_point_same(receipt.vertices[i],receipt.anchor,other.vertices[i],other.anchor)),_=>false}{return false;}
            }
        }
        let opposite = self
            .normal
            .iter()
            .zip(plane.normal)
            .all(|(&a, b)| same(a, -b));
        let equal = self
            .normal
            .iter()
            .zip(plane.normal)
            .all(|(&a, b)| same(a, b));
        if !equal && !opposite {
            return false;
        }
        let offset = if opposite {
            -plane.offset
        } else {
            plane.offset
        };
        if !same(self.offset, offset) {
            return false;
        }
        let axis = (0..3).find(|&a| {
            (same(self.normal[a], S::one()) || same(self.normal[a], -S::one()))
                && (0..3).all(|k| k == a || same(self.normal[k], S::zero()))
        });
        let (aa,a,ba,b)=match (self.local_origin,plane.local_origin) {
            (Some((aa,a)),Some((ba,b)))=>(aa,a,ba,b),
            (None,None)=>([0.;3],self.origin,[0.;3],plane.origin),
            _=>return false,
        };
        if let Some(axis)=axis {
            let mut n=[0.;3]; n[axis]=1.;
            crate::native_exact_affine_predicate::exact_framed_plane_sign(n,a.map(|v|v.value()),aa,b.map(|v|v.value()),ba,0.)==Ok(0)
                && a[axis].geometry_first_order_directions().zip(b[axis].geometry_first_order_directions()).is_some_and(|(x,y)|x.iter().chain(y.iter()).all(|v|v.is_finite()) && x==y)
        } else { framed_point_same(a,aa,b,ba) }
    }
}
pub(crate) fn polygon_vertices_in_frame<S: Scalar>(
    face: &CutPolygon<S>,
    anchor: [f64; 3],
) -> Vec<[S; 3]> {
    match &face.local_geometry {
        Some(local) => local
            .vertices_m
            .iter()
            .map(|p| std::array::from_fn(|k| p[k] + (local.anchor_m[k] - anchor[k])))
            .collect(),
        None => face
            .vertices_m
            .iter()
            .map(|p| std::array::from_fn(|k| p[k] - anchor[k]))
            .collect(),
    }
}
pub(crate) fn polygon_plane_origin_in_frame<S: Scalar>(
    face: &CutPolygon<S>,
    anchor: [f64; 3],
) -> [S; 3] {
    match &face.local_geometry {
        Some(local) => {
            std::array::from_fn(|k| local.plane_origin_m[k] + (local.anchor_m[k] - anchor[k]))
        }
        None => std::array::from_fn(|k| face.plane_origin_m[k] - anchor[k]),
    }
}
pub(crate) fn bind_polygon_frame<S: TransientGasScalar>(
    face: &mut CutPolygon<S>,
    anchor: [f64; 3],
) -> CaeResult<()> {
    if anchor.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract("cut local frame is nonfinite"));
    }
    let vertices_m = polygon_vertices_in_frame(face, anchor);
    let plane_origin_m = polygon_plane_origin_in_frame(face, anchor);
    if vertices_m
        .iter()
        .flatten()
        .chain(plane_origin_m.iter())
        .any(|v| !v.finite_first_order() || v.geometry_first_order_directions().is_none())
    {
        return Err(CaeError::contract(
            "cut local geometry has incomplete channels",
        ));
    }
    let incidences = face.local_geometry.as_ref().map_or_else(
        || vec![Vec::new(); vertices_m.len()],
        |g| g.incidences.clone(),
    );
    let plane_incidence=face.local_geometry.as_ref().and_then(|g|g.plane_incidence.clone());
    let point_constructions=face.local_geometry.as_ref().map_or_else(||vec![PointConstruction::Leaf;vertices_m.len()],|g|g.point_constructions.clone());
    if point_constructions.len()!=vertices_m.len(){return Err(CaeError::contract("cut local construction shape differs"));}
    face.local_geometry = Some(CutLocalGeometry {
        point_constructions,
        plane_incidence,
        anchor_m: anchor,
        vertices_m,
        plane_origin_m,
        incidences,
    });
    Ok(())
}
pub(crate) fn local_polygon_area<S: Scalar>(face: &CutPolygon<S>) -> S {
    super::area(
        face.local_geometry
            .as_ref()
            .map_or(face.vertices_m.as_slice(), |g| g.vertices_m.as_slice()),
    )
}
pub(crate) fn local_positive_polygon<S: Scalar>(face: &CutPolygon<S>) -> bool {
    let p = face
        .local_geometry
        .as_ref()
        .map_or(face.vertices_m.as_slice(), |g| g.vertices_m.as_slice());
    p.len() >= 3 && super::area(p).value() > 0.
}

fn positive_nodes<S: TransientGasScalar>(nodes: &[LocalNode<S>]) -> CaeResult<bool> {
    if nodes.len() < 3 {
        return Ok(false);
    }
    let points = nodes.iter().map(|p| p.point).collect::<Vec<_>>();
    let vector = super::area_vector(&points);
    if vector
        .iter()
        .any(|v| !v.finite_first_order() || v.geometry_first_order_directions().is_none())
    {
        return Err(CaeError::contract("cut local area has incomplete channels"));
    }
    if vector.iter().all(|v| v.value() == 0.) {
        if vector.iter().any(|v| {
            v.geometry_first_order_directions()
                .is_some_and(|d| d.iter().any(|v| *v != 0.))
        }) {
            return Err(CaeError::contract(
                "cut local zero area has nonzero channels",
            ));
        }
        return Ok(false);
    }
    Ok(true)
}
#[derive(Clone)]
struct LocalNode<S: Scalar> {
    construction:PointConstruction<S>,
    point: [S; 3],
    incidences: Vec<CutPlaneIncidence<S>>,
}
fn point_same<S: TransientGasScalar>(a: [S; 3], b: [S; 3]) -> bool {
    (0..3).all(|k| same(a[k], b[k]))
}
fn incidence_plane<S: Scalar>(p: &CutPlaneIncidence<S>) -> Plane<S> {
    Plane { native_levelset:p.native_levelset.clone(), subdomain_face:p.subdomain_face.clone(),
        native_face: p.native_face.clone(), normal: p.normal, origin: p.origin,
        offset: p.offset, boundary: p.boundary.clone(), local_origin: p.local_origin,
    }
}
fn static_cell_coordinate<S: TransientGasScalar>(p: &CutPlaneIncidence<S>, anchor: [f64;3]) -> Option<(usize,S)> {
    let CutBoundary::Cell {axis,side}=p.boundary else{return None;};
    if axis>=3 || side>1 || p.native_face.is_some() || p.subdomain_face.is_some() || p.native_levelset.is_some() {return None;}
    let constant=|v:S|v.finite_first_order() && v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|*x==0.));
    if !p.normal.iter().copied().all(constant) || !constant(p.offset)
        || !((same(p.normal[axis],S::one()) || same(p.normal[axis],-S::one()))
             && (0..3).all(|k|k==axis || same(p.normal[k],S::zero()))) {return None;}
    let (frame,origin)=p.local_origin.unwrap_or(([0.;3],p.origin));
    if frame.iter().any(|v|!v.is_finite()) || !origin.iter().all(|v|v.finite_first_order() && v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite()))) || !constant(origin[axis]) {return None;}
    let coordinate=origin[axis]+(frame[axis]-anchor[axis])-p.offset/p.normal[axis];
    constant(coordinate).then_some((axis,coordinate))
}
fn clip_cell_coordinate<S:TransientGasScalar>(plane:&Plane<S>,anchor:[f64;3])->Option<(usize,S)> {
    if !same(plane.offset,S::zero()) || !plane.local_origin.is_some_and(|(frame,_)|frame==anchor){return None;}
    static_cell_coordinate(&CutPlaneIncidence::from_plane(plane),anchor)
}
fn qualify_corner_view<S: TransientGasScalar>(point:[S;3],plane:&Plane<S>,anchor:[f64;3])->CaeResult<()> {
    let refuse=||CaeError::contract("cut local corner view differs from original constraints");
    let origin=plane_origin_in_frame(plane,anchor);
    let mut residual=plane.offset;
    let mut primal=plane.offset.value().abs();
    let inputs=point.iter().chain(origin.iter()).chain(plane.normal.iter()).copied().chain([plane.offset]).collect::<Vec<_>>();
    let rows=inputs.iter().map(|v|v.geometry_first_order_directions()).collect::<Option<Vec<_>>>().ok_or_else(refuse)?;
    let width=rows.iter().map(Vec::len).max().unwrap_or(0);
    if inputs.iter().zip(&rows).any(|(v,d)|!v.finite_first_order() || d.iter().any(|x|!x.is_finite()) || (d.len()!=width && !(d.is_empty() && !v.has_direction()))) {return Err(refuse());}
    let mut condition=plane.offset.geometry_first_order_directions().ok_or_else(refuse)?.into_iter().map(f64::abs).collect::<Vec<_>>();
    condition.resize(width,0.);
    for k in 0..3 {
        residual=residual+plane.normal[k]*point[k]-plane.normal[k]*origin[k];
        for factor in [point[k],origin[k]] {
            primal+=crate::gas_transient_kinetic::geometry_conditioning_product(&[plane.normal[k].value().abs(),factor.value().abs()]).ok_or_else(refuse)?;
            let mut row=S::geometry_product_factors_direction_bounds(&[plane.normal[k],factor]).ok_or_else(refuse)?;
            if row.len()!=width && !(row.is_empty() && !plane.normal[k].has_direction() && !factor.has_direction()){return Err(refuse());}
            row.resize(width,0.);
            for (bound,term) in condition.iter_mut().zip(row) {*bound+=term;}
        }
    }
    let mut channels=residual.geometry_first_order_directions().ok_or_else(refuse)?;
    if !residual.finite_first_order() || !primal.is_finite() || (channels.len()!=width && !(channels.is_empty() && !residual.has_direction())) {return Err(refuse());}
    channels.resize(width,0.);
    let gamma=64.*f64::EPSILON/(1.-64.*f64::EPSILON);
    let qualified=|value:f64,bound:f64|value.is_finite() && bound.is_finite() && bound>=0. && if bound==0. {value==0.} else {value.abs()<=gamma*bound};
    if !qualified(residual.value(),primal) || !channels.into_iter().zip(condition).all(|(value,bound)|qualified(value,bound)) {return Err(refuse());}
    Ok(())
}
fn authentic_native_point_incidence<S:TransientGasScalar>(p:&CutPlaneIncidence<S>,point:[S;3],anchor:[f64;3])->bool {
    let Some(receipt)=&p.native_face else{return false;};
    let plane=incidence_plane(p);
    if !receipt.valid(&plane) || !same(p.offset,S::zero())
        || receipt.anchor.iter().any(|v|!v.is_finite())
        || receipt.vertices.iter().flatten().any(|v|!v.finite_first_order() || !v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite()))) {return false;}
    receipt.vertices.iter().any(|vertex|framed_point_same(point,anchor,*vertex,receipt.anchor))
        || maintained_plane_origins(p.normal,point,anchor,receipt.vertices[0],receipt.anchor)
}
fn subdomain_receipt_qualified<S:TransientGasScalar>(p:&CutPlaneIncidence<S>)->bool {p.subdomain_face.as_ref().is_some_and(|r|r.valid(&incidence_plane(p)) && (r.native_support.is_some() || same(p.offset,S::zero())))}
fn subdomain_owns_vertex<S:TransientGasScalar>(p:&CutPlaneIncidence<S>,point:[S;3],anchor:[f64;3])->bool {subdomain_receipt_qualified(p) && p.subdomain_face.as_ref().unwrap().vertices.iter().any(|v|framed_point_same(point,anchor,*v,p.subdomain_face.as_ref().unwrap().anchor))}
fn authentic_subdomain_point_incidence<S:TransientGasScalar>(p:&CutPlaneIncidence<S>,point:[S;3],anchor:[f64;3])->bool {
    if !subdomain_receipt_qualified(p){return false;}
    let r=p.subdomain_face.as_ref().unwrap();
    subdomain_owns_vertex(p,point,anchor) || maintained_plane_origins(p.normal,point,anchor,r.vertices[0],r.anchor)
}
fn subdomain_native_plane_equivalence<S:TransientGasScalar>(p:&CutPlaneIncidence<S>,other:&Plane<S>)->bool {
    if !subdomain_receipt_qualified(p){return false;}
    if let Some(support)=p.subdomain_face.as_ref().and_then(|r|r.native_support.as_ref()){return support.plane().is_ok_and(|plane|CutPlaneIncidence::from_plane(&plane).matches(other));}
    let (Some(a),Some(b))=(&p.subdomain_face,&other.native_face) else{return false;};
    if !b.valid(other) || !same(other.offset,S::zero()){return false;}
    let opposite=(0..3).all(|k|same(p.normal[k],-other.normal[k]));
    if !(opposite || (0..3).all(|k|same(p.normal[k],other.normal[k]))){return false;}
    let mut used=[false;3];
    for point in a.vertices {let Some(j)=(0..3).find(|&j|!used[j] && framed_point_same(point,a.anchor,b.vertices[j],b.anchor)) else{return false;};used[j]=true;}
    true
}
#[derive(Clone)]
enum OwnedCornerKind<S:Scalar> {
    SourceVertex {source:NativeNodeSource<S>,construction:Arc<ClipPointConstruction<S>>},
    Vertex {body:usize,node:usize,point:[S;3],anchor:[f64;3]},
    Edge {body:usize,nodes:[usize;2],points:[[S;3];2],anchor:[f64;3],cell:Plane<S>},
    LevelsetEdge {body:usize,nodes:[usize;2],points:[[S;3];2],anchor:[f64;3],values:[S;2],iso:f64},
    LevelsetFaceCell {body:usize,nodes:[usize;3],points:[[S;3];3],anchor:[f64;3],values:[S;3],iso:f64,cell:Plane<S>},
    ChartVertex {region:usize,id:usize,source:NativeChartPointSource<S>,construction:Arc<ClipPointConstruction<S>>},
    ChartEdge {region:usize,ids:[usize;2],sources:[NativeChartPointSource<S>;2],cell:Plane<S>,construction:Arc<ClipPointConstruction<S>>},
    NativeChartCell {construction:Arc<ClipPointConstruction<S>>},
    Planes,
}
#[derive(Clone)]
struct OwnedCornerRecipe<S:Scalar> {
    point:[S;3], planes:[Plane<S>;3], kind:OwnedCornerKind<S>,
}
fn native_receipt_qualified<S:TransientGasScalar>(p:&CutPlaneIncidence<S>)->bool {
    p.native_face.as_ref().is_some_and(|r|r.valid(&incidence_plane(p))
        && r.anchor.iter().all(|x|x.is_finite())
        && r.vertices.iter().flatten().all(|v|v.finite_first_order() && v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite()))))
}
fn qualify_original_vertex_view<S:TransientGasScalar>(point:[S;3],anchor:[f64;3],vertex:[S;3],frame:[f64;3])->CaeResult<()> {
    let refuse=||CaeError::contract("cut local corner view differs from original vertex");
    if anchor.iter().chain(frame.iter()).any(|v|!v.is_finite()){return Err(refuse());}
    let gamma=64.*f64::EPSILON/(1.-64.*f64::EPSILON);
    for k in 0..3 {
        let a=point[k];let b=vertex[k];
        let ar=a.geometry_first_order_directions().ok_or_else(refuse)?;
        let br=b.geometry_first_order_directions().ok_or_else(refuse)?;
        let width=ar.len().max(br.len());
        if !a.finite_first_order() || !b.finite_first_order()
            || ar.iter().chain(br.iter()).any(|v|!v.is_finite())
            || (ar.len()!=width && !(ar.is_empty() && !a.has_direction()))
            || (br.len()!=width && !(br.is_empty() && !b.has_direction())){return Err(refuse());}
        let shift=frame[k]-anchor[k];
        let residual=a.value()-(b.value()+shift);
        let condition=a.value().abs()+b.value().abs()+shift.abs();
        if !shift.is_finite() || !residual.is_finite() || !condition.is_finite()
            || if condition==0. {residual!=0.} else {residual.abs()>gamma*condition}{return Err(refuse());}
        for j in 0..width {
            let x=ar.get(j).copied().unwrap_or(0.);let y=br.get(j).copied().unwrap_or(0.);
            let residual=x-y;let condition=x.abs()+y.abs();
            if !residual.is_finite() || !condition.is_finite()
                || if condition==0. {residual!=0.} else {residual.abs()>gamma*condition}{return Err(refuse());}
        }
    }
    Ok(())
}
fn qualify_native_corner_view<S:TransientGasScalar>(point:[S;3],incidence:&CutPlaneIncidence<S>,anchor:[f64;3])->CaeResult<()> {
    let plane=incidence_plane(incidence);
    let finite=|v:S|v.finite_first_order() && v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite()));
    if native_receipt_qualified(incidence) && same(incidence.offset,S::zero())
        && point.iter().copied().all(finite) && plane.normal.iter().copied().all(finite)
        && plane.origin.iter().copied().all(finite)
        && plane.local_origin.is_none_or(|(frame,origin)|frame.iter().all(|v|v.is_finite()) && origin.iter().copied().all(finite))
        && {let face=incidence.native_face.as_ref().unwrap();if let Some(source)=&face.source {face.nodes.iter().any(|id|source.nodes.iter().position(|n|n==id).is_some_and(|i|source.points[i].point_in_frame(anchor).is_ok_and(|p|point_same(point,p))))}else{face.vertices.iter().any(|vertex|framed_point_same(point,anchor,*vertex,face.anchor))}}{return Ok(());}
    qualify_corner_view(point,&plane,anchor)
}
fn native_owns_vertex<S:TransientGasScalar>(p:&CutPlaneIncidence<S>,body:usize,node:usize,point:[S;3],anchor:[f64;3])->bool {
    if !native_receipt_qualified(p) || !same(p.offset,S::zero()){return false;}
    let r=p.native_face.as_ref().unwrap();
    r.body==body && r.nodes.iter().position(|id|*id==node).is_some_and(|i|if let Some(source)=&r.source {source.nodes.iter().position(|id|*id==node).is_some_and(|j|source.points[j].point_in_frame(anchor).is_ok_and(|p|point_same(point,p)))}else{framed_point_same(point,anchor,r.vertices[i],r.anchor)})
}
fn vertex_satisfies_cell<S:TransientGasScalar>(p:&CutPlaneIncidence<S>,point:[S;3],frame:[f64;3],anchor:[f64;3])->bool {
    let Some((axis,_))=static_cell_coordinate(p,anchor) else{return false;};
    let (oa,origin)=p.local_origin.unwrap_or(([0.;3],p.origin));
    crate::native_exact_affine_predicate::exact_framed_plane_sign(p.normal.map(|v|v.value()),point.map(|v|v.value()),frame,origin.map(|v|v.value()),oa,p.offset.value())==Ok(0)
        && point[axis].geometry_first_order_directions().is_some_and(|d|d.iter().all(|v|*v==0.))
}
fn levelset_owns_vertex<S:TransientGasScalar>(p:&CutPlaneIncidence<S>,body:usize,node:usize,point:[S;3],anchor:[f64;3])->bool {
    let Some(d)=&p.native_levelset else{return false;};d.valid(&incidence_plane(p)) && d.body==body && d.nodes.iter().position(|id|*id==node).is_some_and(|i|same(d.values[i],S::from_f64(d.iso)) && if let Some(source)=&d.source {source.points[i].point_in_frame(anchor).is_ok_and(|p|point_same(point,p))}else{framed_point_same(d.vertices[i],d.anchor,point,anchor)})
}
fn source_incidence_in_frame<S:TransientGasScalar>(incidence:&CutPlaneIncidence<S>,anchor:[f64;3])->CaeResult<CutPlaneIncidence<S>> {
    if incidence.native_face.as_ref().is_some_and(|p|p.source.is_some() && !p.valid(&incidence_plane(incidence))) || incidence.native_levelset.as_ref().is_some_and(|p|p.source.is_some() && !p.valid(&incidence_plane(incidence))){return Err(CaeError::contract("native corner input source view is invalid"));}
    let mut live=incidence.clone();
    let origin=if let Some(face)=&mut live.native_face {
        if let Some(source)=&face.source {
            source.validate()?;
            let indices=source.face_indices(face.nodes).ok_or_else(||CaeError::contract("native corner source face is absent"))?;
            let points=source.vertices_in_frame(anchor)?;
            face.vertices=indices.map(|i|points[i]);face.anchor=anchor;Some(face.vertices[0])
        }else{None}
    }else if let Some(density)=&mut live.native_levelset {
        if let Some(source)=&density.source {source.validate()?;density.vertices=source.vertices_in_frame(anchor)?;density.anchor=anchor;Some(density.vertices[0])}else{None}
    }else{None};
    if let Some(origin)=origin {live.local_origin=Some((anchor,origin));live.origin=std::array::from_fn(|k|origin[k]+anchor[k]);}
    if (live.native_face.as_ref().is_some_and(|p|p.source.is_some()) || live.native_levelset.as_ref().is_some_and(|p|p.source.is_some())) && !live.native_face.as_ref().map_or_else(||live.native_levelset.as_ref().is_some_and(|p|p.valid(&incidence_plane(&live))),|p|p.valid(&incidence_plane(&live))){return Err(CaeError::contract("native corner original source view is invalid"));}
    Ok(live)
}
fn source_vertex_in_subdomain<S:TransientGasScalar>(source:&NativeNodeSource<S>,plane:&Plane<S>)->bool {
    let Some(receipt)=&plane.subdomain_face else{return false;};
    if !receipt.valid(plane) || !same(plane.offset,S::zero()){return false;}
    receipt.chart_sources.as_ref().is_some_and(|charts|charts.iter().any(|chart|{
        let Some(feature)=chart.boundary_feature()else{return false;};
        matches!(feature.feature,crate::moving_contact::surface_map::SurfaceFeature::Vertex(id) if id==source.node)
            && feature.weights().is_ok() && feature.points.len()==1 && feature.scales.len()==1
            && feature.scales[0].iter().all(|pair|pair[0].to_bits()==pair[1].to_bits()) && feature.points[0].same(source)
    }))
}
fn source_vertex_owns_incidence<S:TransientGasScalar>(source:&NativeNodeSource<S>,row:&CutPlaneIncidence<S>,anchor:[f64;3])->bool {
    if !same(row.offset,S::zero()){return false;}
    if let Some((axis,coordinate))=static_cell_coordinate(row,anchor){return source.point_in_frame(anchor).is_ok_and(|point|same(point[axis],coordinate));}
    if !same(row.offset,S::zero()){return false;}
    if native_receipt_qualified(row) {
        let face=row.native_face.as_ref().unwrap();
        return face.body==source.body && face.nodes.contains(&source.node) && face.source.as_ref().is_some_and(|owner|owner.nodes.iter().position(|id|*id==source.node).is_some_and(|u|owner.points[u].same(source)));
    }
    if let Some(density)=&row.native_levelset {
        return density.valid(&incidence_plane(row)) && density.body==source.body && density.source.as_ref().is_some_and(|owner|owner.nodes.iter().position(|id|*id==source.node).is_some_and(|u|owner.points[u].same(source) && same(density.values[u],S::from_f64(density.iso))));
    }
    source_vertex_in_subdomain(source,&incidence_plane(row))
}
fn source_vertex_original_rank<S:TransientGasScalar>(vertex:&NativeNodeSource<S>,rows:&[CutPlaneIncidence<S>],anchor:[f64;3])->bool {
    let charts=rows.iter().filter_map(|p|p.subdomain_face.as_ref().filter(|r|r.valid(&incidence_plane(p)) && same(p.offset,S::zero()) && r.chart_sources.is_some())).collect::<Vec<_>>();
    for i in 0..charts.len(){for j in i+1..charts.len(){
        let a=charts[i].chart_sources.as_ref().unwrap();let b=charts[j].chart_sources.as_ref().unwrap();
        let shared=a.iter().filter(|s|b.iter().any(|v|s.same(v))).collect::<Vec<_>>();if shared.len()!=2{continue;}
        let mut original=Vec::new();let mut valid=true;
        for expression in &shared {let Some(feature)=expression.boundary_feature()else{valid=false;break;};if !matches!(feature.feature,crate::moving_contact::surface_map::SurfaceFeature::Vertex(_)) || feature.points.len()!=1 || feature.scales.len()!=1 || !feature.scales[0].iter().all(|p|p[0].to_bits()==p[1].to_bits()){valid=false;break;}original.push(&feature.points[0]);}
        if !valid || !original.iter().any(|p|p.same(vertex)){continue;}
        let mut sources=Vec::new();for expression in a.iter().chain(b.iter()){if !sources.iter().any(|v:&NativeChartPointSource<S>|v.same(expression)){sources.push(expression.clone());}}let Ok(sources):Result<[NativeChartPointSource<S>;4],_>=sources.try_into()else{continue;};
        if !matches!(crate::native_geometry_construction_error::chart_tetra_expression_rank(&sources,anchor),Ok(-1)|Ok(1)){continue;}
        for row in rows {
            if !native_receipt_qualified(row) || !same(row.offset,S::zero()){continue;}
            let face=row.native_face.as_ref().unwrap();let Some(source)=&face.source else{continue;};
            if source.validate().is_err(){continue;}
            if original.iter().all(|p|source.nodes.iter().position(|id|*id==p.node).is_some_and(|u|source.points[u].same(p))) && original.iter().filter(|p|face.nodes.contains(&p.node)).count()==1 && face.nodes.contains(&vertex.node){return true;}
            if !face.nodes.iter().any(|id|source.nodes.iter().position(|n|n==id).is_some_and(|u|source.points[u].same(vertex))){continue;}
            let Some(other)=shared.iter().find(|expression|expression.boundary_feature().is_some_and(|feature|!feature.points[0].same(vertex)))else{continue;};
            if matches!(crate::native_geometry_construction_error::native_chart_point_expression_rank(source,face.nodes,other,anchor),Ok(-1)|Ok(1)){return true;}
        }
    }}
    let natives=rows.iter().filter(|row|native_receipt_qualified(row) && same(row.offset,S::zero())).collect::<Vec<_>>();
    for i in 0..natives.len(){for j in i+1..natives.len(){
        let a=natives[i].native_face.as_ref().unwrap();let b=natives[j].native_face.as_ref().unwrap();
        let (Some(source),Some(other_source))=(&a.source,&b.source)else{continue;};
        if source.validate().is_err() || other_source.validate().is_err() || !source.same(other_source){continue;}
        let shared=a.nodes.iter().filter(|id|b.nodes.contains(id)).copied().collect::<Vec<_>>();
        let mut union=a.nodes.into_iter().chain(b.nodes).collect::<Vec<_>>();union.sort_unstable();union.dedup();
        if shared.len()!=2 || union.len()!=4 || !union.iter().all(|id|source.nodes.contains(id)){continue;}
        let Some(candidate)=shared.iter().position(|id|source.nodes.iter().position(|n|n==id).is_some_and(|u|source.points[u].same(vertex)))else{continue;};
        let other=shared[1-candidate];
        for row in rows {
            let Some(chart)=&row.subdomain_face else{continue;};
            if !source_vertex_in_subdomain(vertex,&incidence_plane(row)){continue;}
            let Some(expressions)=&chart.chart_sources else{continue;};
            if matches!(crate::native_geometry_construction_error::chart_native_point_expression_rank(source,other,expressions,anchor),Ok(-1)|Ok(1)){return true;}
        }
    }}
    false
}
fn owned_corner_recipe<S:TransientGasScalar>(node:&LocalNode<S>,anchor:[f64;3])->CaeResult<Option<OwnedCornerRecipe<S>>> {
    if anchor.iter().any(|v|!v.is_finite()){return Err(CaeError::contract("cut local corner frame is nonfinite"));}
    let source_incidences=node.incidences.iter().map(|p|source_incidence_in_frame(p,anchor)).collect::<CaeResult<Vec<_>>>()?;
    let source_node=LocalNode{point:node.point,incidences:source_incidences,construction:node.construction.clone()};
    let node=&source_node;
    let cells=node.incidences.iter().filter_map(|p|static_cell_coordinate(p,anchor).map(|(axis,x)|(axis,x,p))).collect::<Vec<_>>();
    let mut natives=node.incidences.iter().filter(|p|native_receipt_qualified(p)).collect::<Vec<_>>();
    natives.sort_by_key(|p|match p.boundary {CutBoundary::Native{body,element,face,physical}=>(body,element,face,physical),_=>(usize::MAX,usize::MAX,None,false)});
    let mut recipe=None;
    let mut levelsets=node.incidences.iter().filter(|p|p.native_levelset.as_ref().is_some_and(|r|r.valid(&incidence_plane(p)))).collect::<Vec<_>>();
    levelsets.sort_by_key(|p|match p.boundary{CutBoundary::Native{body,element,..}=>(body,element),_=>(usize::MAX,usize::MAX)});
    'levelset_recipe:for density in levelsets.iter().copied() {for i in 0..natives.len(){for j in i+1..natives.len(){
        let ra=natives[i].native_face.as_ref().unwrap();let rb=natives[j].native_face.as_ref().unwrap();let d=density.native_levelset.as_ref().unwrap();
        if ra.body!=rb.body || ra.body!=d.body || !same(natives[i].offset,S::zero()) || !same(natives[j].offset,S::zero()){continue;}
        let mut shared=ra.nodes.iter().enumerate().filter_map(|(u,id)|rb.nodes.iter().position(|v|v==id).map(|v|(*id,u,v))).collect::<Vec<_>>();shared.sort_by_key(|(id,_,_)|*id);
        if shared.len()!=2 || !shared.iter().all(|(_,u,v)|framed_point_same(ra.vertices[*u],ra.anchor,rb.vertices[*v],rb.anchor)){continue;}
        let Some(ta)=(0..3).find(|i|!shared.iter().any(|(_,u,_)|u==i))else{continue;};let Some(tb)=(0..3).find(|i|!shared.iter().any(|(_,_,v)|v==i))else{continue;};
        let primitive=[ra.vertices[shared[0].1],ra.vertices[shared[1].1],ra.vertices[ta],rb.vertices[tb]].map(|p|p.map(|v|v.value()));
        if !matches!(crate::native_exact_affine_predicate::exact_framed_tetra_sign(primitive,[ra.anchor,ra.anchor,ra.anchor,rb.anchor]),Ok(-1)|Ok(1)){continue;}
        let (Some(u),Some(v))=(d.nodes.iter().position(|id|*id==shared[0].0),d.nodes.iter().position(|id|*id==shared[1].0))else{continue;};
        let points=[d.vertices[u],d.vertices[v]];
        if !(0..2).all(|k|framed_point_same(points[k],d.anchor,ra.vertices[shared[k].1],ra.anchor)){continue;}
        let values=[d.values[u],d.values[v]];let denominator=values[1]-values[0];
        if denominator.value()==0. || !denominator.finite_first_order() || !denominator.geometry_first_order_directions().is_some_and(|d|d.iter().all(|v|v.is_finite())){continue;}
        let t=(S::from_f64(d.iso)-values[0])/denominator;
        let local:[[S;3];2]=points.map(|p|std::array::from_fn(|k|p[k]+(d.anchor[k]-anchor[k])));
        let point=std::array::from_fn(|k|local[0][k]+(local[1][k]-local[0][k])*t);
        recipe=Some(OwnedCornerRecipe{point,planes:[incidence_plane(natives[i]),incidence_plane(natives[j]),incidence_plane(density)],kind:OwnedCornerKind::LevelsetEdge{body:d.body,nodes:[shared[0].0,shared[1].0],points,anchor:d.anchor,values,iso:d.iso}});break 'levelset_recipe;
    }}}
    if recipe.is_none(){
        'face_levelset_recipe:for density in levelsets.iter().copied(){for native in &natives {
            let d=density.native_levelset.as_ref().unwrap();let r=native.native_face.as_ref().unwrap();
            if r.body!=d.body || !same(native.offset,S::zero()){continue;}
            let indices=r.nodes.map(|id|d.nodes.iter().position(|n|*n==id));
            if indices.iter().any(Option::is_none){continue;}
            let indices=indices.map(Option::unwrap);
            if !(0..3).all(|i|framed_point_same(r.vertices[i],r.anchor,d.vertices[indices[i]],d.anchor)){continue;}
            let points=r.vertices;let values=indices.map(|i|d.values[i]);
            let local:[[S;3];3]=points.map(|p|std::array::from_fn(|k|p[k]+(r.anchor[k]-anchor[k])));
            for (axis,coordinate,cell) in &cells {
                let primitive=std::array::from_fn(|i|if i<3{[points[i][*axis].value(),values[i].value(),0.]}else{[0.,0.,1.]});
                let frames=std::array::from_fn(|i|if i<3{[r.anchor[*axis],0.,0.]}else{[0.;3]});
                if !matches!(crate::native_exact_affine_predicate::exact_framed_tetra_sign(primitive,frames),Ok(-1)|Ok(1)){continue;}
                let a=local[1][*axis]-local[0][*axis];let b=local[2][*axis]-local[0][*axis];
                let c=values[1]-values[0];let e=values[2]-values[0];
                let determinant=a*e-b*c;
                if determinant.value()==0. || !determinant.finite_first_order() || !determinant.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite())){continue;}
                let f=*coordinate-local[0][*axis];let g=S::from_f64(d.iso)-values[0];
                let u=(f*e-b*g)/determinant;let v=(a*g-f*c)/determinant;
                let mut point=std::array::from_fn(|k|local[0][k]+(local[1][k]-local[0][k])*u+(local[2][k]-local[0][k])*v);
                point[*axis]=*coordinate;
                recipe=Some(OwnedCornerRecipe{point,planes:[incidence_plane(native),incidence_plane(density),incidence_plane(cell)],kind:OwnedCornerKind::LevelsetFaceCell{body:r.body,nodes:r.nodes,points,anchor:r.anchor,values,iso:d.iso,cell:incidence_plane(cell)}});
                break 'face_levelset_recipe;
            }
        }}
    }
    if recipe.is_none(){
    'edge_recipe:for i in 0..natives.len(){for j in i+1..natives.len(){
        let ra=natives[i].native_face.as_ref().unwrap();let rb=natives[j].native_face.as_ref().unwrap();
        if ra.body!=rb.body || !same(natives[i].offset,S::zero()) || !same(natives[j].offset,S::zero()){continue;}
        let mut shared=ra.nodes.iter().enumerate().filter_map(|(u,id)|rb.nodes.iter().position(|v|v==id).map(|v|(*id,u,v))).collect::<Vec<_>>();
        shared.sort_by_key(|(id,_,_)|*id);
        if shared.len()!=2 || !shared.iter().all(|(_,u,v)|framed_point_same(ra.vertices[*u],ra.anchor,rb.vertices[*v],rb.anchor)){continue;}
        let Some(ta)=(0..3).find(|i|!shared.iter().any(|(_,u,_)|u==i)) else{continue;};
        let Some(tb)=(0..3).find(|i|!shared.iter().any(|(_,_,v)|v==i)) else{continue;};
        let primitive=[ra.vertices[shared[0].1],ra.vertices[shared[1].1],ra.vertices[ta],rb.vertices[tb]].map(|p|p.map(|v|v.value()));
        if !matches!(crate::native_exact_affine_predicate::exact_framed_tetra_sign(primitive,[ra.anchor,ra.anchor,ra.anchor,rb.anchor]),Ok(-1)|Ok(1)){continue;}
        for (axis,coordinate,cell) in &cells {
            let k=(*axis+1)%3;let l=(*axis+2)%3;
            if !matches!(crate::native_exact_affine_predicate::exact_dot_sign([natives[i].normal[k].value(),-natives[i].normal[l].value(),0.],[natives[j].normal[l].value(),natives[j].normal[k].value(),0.]),Ok(-1)|Ok(1)){continue;}
            let points=[ra.vertices[shared[0].1],ra.vertices[shared[1].1]];
            let local=points.map(|p|std::array::from_fn(|k|p[k]+(ra.anchor[k]-anchor[k])));
            let edge=super::sub(local[1],local[0]);
            if edge[*axis].value()==0. || !edge[*axis].finite_first_order(){continue;}
            let t=(*coordinate-local[0][*axis])/edge[*axis];
            let mut point=std::array::from_fn(|k|local[0][k]+edge[k]*t);point[*axis]=*coordinate;
            recipe=Some(OwnedCornerRecipe {point,planes:[incidence_plane(cell),incidence_plane(natives[i]),incidence_plane(natives[j])],kind:OwnedCornerKind::Edge {body:ra.body,nodes:[shared[0].0,shared[1].0],points,anchor:ra.anchor,cell:incidence_plane(cell)}});
            break 'edge_recipe;
        }
    }}
    }
    if recipe.is_none(){
        'plane_recipe:for native in &natives {for a in 0..3 {for b in a+1..3 {
            let Some((_,x,ca))=cells.iter().find(|(axis,_,_)|*axis==a) else{continue;};
            let Some((_,y,cb))=cells.iter().find(|(axis,_,_)|*axis==b) else{continue;};
            let k=3-a-b;if native.normal[k].value()==0.{continue;}
            let r=native.native_face.as_ref().unwrap();
            let mut fourth=r.vertices[0].map(|v|v.value());fourth[k]=if fourth[k]==0.{1.}else{0.};
            let primitive=[r.vertices[0].map(|v|v.value()),r.vertices[1].map(|v|v.value()),r.vertices[2].map(|v|v.value()),fourth];
            if !matches!(crate::native_exact_affine_predicate::exact_framed_tetra_sign(primitive,[r.anchor;4]),Ok(-1)|Ok(1)){continue;}
            let plane=incidence_plane(native);let origin=plane_origin_in_frame(&plane,anchor);
            let mut point=node.point;point[a]=*x;point[b]=*y;
            point[k]=origin[k]-(native.normal[a]*(*x-origin[a])+native.normal[b]*(*y-origin[b])+native.offset)/native.normal[k];
            recipe=Some(OwnedCornerRecipe {point,planes:[incidence_plane(ca),incidence_plane(cb),plane],kind:OwnedCornerKind::Planes});
            break 'plane_recipe;
        }}}
    }
    if recipe.is_none(){
        'native_vertex:for i in 0..natives.len(){for j in i+1..natives.len(){for k in j+1..natives.len(){
            let rows=[natives[i],natives[j],natives[k]];
            if !rows.iter().all(|p|same(p.offset,S::zero())){continue;}
            let faces=rows.map(|p|p.native_face.as_ref().unwrap());
            let owner=|p:&CutPlaneIncidence<S>|match p.boundary {CutBoundary::Native{body,element,..}=>Some((body,element)),_=>None};
            if owner(rows[0])!=owner(rows[1]) || owner(rows[0])!=owner(rows[2]){continue;}
            let mut ids=faces.iter().flat_map(|f|f.nodes).collect::<Vec<_>>();ids.sort_unstable();ids.dedup();
            if ids.len()!=4 || faces[0].nodes==faces[1].nodes || faces[0].nodes==faces[2].nodes || faces[1].nodes==faces[2].nodes{continue;}
            let common=ids.iter().copied().filter(|id|faces.iter().all(|f|f.nodes.contains(id))).collect::<Vec<_>>();
            if common.len()!=1{continue;}
            let mut controls:Vec<([S;3],[f64;3])>=Vec::new();let mut compatible=true;
            for id in &ids {
                let mut reference:Option<([S;3],[f64;3])>=None;
                for face in &faces {if let Some(index)=face.nodes.iter().position(|v|v==id){
                    let point=face.vertices[index];
                    if let Some((old,frame))=reference {if !framed_point_same(point,face.anchor,old,frame){compatible=false;break;}}
                    else{reference=Some((point,face.anchor));}
                }}
                let Some(reference)=reference else{compatible=false;break;};controls.push(reference);
            }
            if !compatible || !matches!(crate::native_exact_affine_predicate::exact_framed_tetra_sign(std::array::from_fn(|n|controls[n].0.map(|v|v.value())),std::array::from_fn(|n|controls[n].1)),Ok(-1)|Ok(1)){continue;}
            let index=ids.iter().position(|id|*id==common[0]).unwrap();let (point,frame)=controls[index];
            recipe=Some(OwnedCornerRecipe{point:std::array::from_fn(|n|point[n]+(frame[n]-anchor[n])),planes:rows.map(incidence_plane),kind:OwnedCornerKind::Vertex{body:faces[0].body,node:common[0],point,anchor:frame}});
            break 'native_vertex;
        }}}
    }
    if recipe.is_none(){
        let charts=node.incidences.iter().filter(|p|subdomain_receipt_qualified(p) && same(p.offset,S::zero()) && p.subdomain_face.as_ref().is_some_and(|r|r.chart_sources.is_some() && r.vertex_ids.is_some())).collect::<Vec<_>>();
        'chart_vertex:for i in 0..charts.len(){for j in i+1..charts.len(){for k in j+1..charts.len(){
            let rows=[charts[i],charts[j],charts[k]];let faces=rows.map(|p|p.subdomain_face.as_ref().unwrap());
            if faces.iter().any(|p|p.region!=faces[0].region) || faces[0].face==faces[1].face || faces[0].face==faces[2].face || faces[1].face==faces[2].face{continue;}
            let face_ids=faces.map(|p|p.vertex_ids.unwrap());
            let common=face_ids[0].iter().filter(|id|face_ids[1].contains(id) && face_ids[2].contains(id)).copied().collect::<Vec<_>>();
            let mut ids=face_ids.into_iter().flatten().collect::<Vec<_>>();ids.sort_unstable();ids.dedup();if common.len()!=1 || ids.len()!=4{continue;}
            let mut sources=Vec::new();let mut valid=true;
            for id in &ids {let mut original:Option<NativeChartPointSource<S>>=None;for f in 0..3 {if let Some(u)=face_ids[f].iter().position(|n|n==id){let current=&faces[f].chart_sources.as_ref().unwrap()[u];if original.as_ref().is_some_and(|p|!p.same(current)){valid=false;break;}original=Some(current.clone());}}if !valid{break;}sources.push(original.unwrap());}
            if !valid{continue;}let sources:[NativeChartPointSource<S>;4]=sources.try_into().map_err(|_|CaeError::contract("cut local chart vertex source shape differs"))?;
            let mut controls=[[S::zero();3];4];for u in 0..4 {match sources[u].point_in_frame(anchor){Ok(point)=>controls[u]=point,Err(_)=>{valid=false;break;}}}if !valid{continue;}
            let represented=crate::native_exact_affine_predicate::exact_framed_tetra_sign(controls.map(|p|p.map(|v|v.value())),[anchor;4]);let original=crate::native_geometry_construction_error::chart_tetra_expression_rank(&sources,anchor);
            if !matches!((represented,original),(Ok(-1),Ok(-1))|(Ok(1),Ok(1))){continue;}
            let u=ids.iter().position(|id|*id==common[0]).unwrap();let point=controls[u];
            let Ok(bounds)=crate::native_geometry_construction_error::chart_point_expression_error(&sources[u],anchor,point)else{continue;};
            let construction=Arc::new(ClipPointConstruction{point,anchor,bounds});
            recipe=Some(OwnedCornerRecipe{point,planes:rows.map(incidence_plane),kind:OwnedCornerKind::ChartVertex{region:faces[0].region,id:common[0],source:sources[u].clone(),construction}});break 'chart_vertex;
        }}}
    }
    if recipe.is_none(){
        let charts=node.incidences.iter().filter(|p|subdomain_receipt_qualified(p) && same(p.offset,S::zero()) && p.subdomain_face.as_ref().is_some_and(|r|r.chart_sources.is_some() && r.vertex_ids.is_some())).collect::<Vec<_>>();
        'chart_edge:for i in 0..charts.len(){for j in i+1..charts.len(){
            let a=charts[i].subdomain_face.as_ref().unwrap();let b=charts[j].subdomain_face.as_ref().unwrap();
            if a.region!=b.region || a.face==b.face{continue;}
            let ai=a.vertex_ids.unwrap();let bi=b.vertex_ids.unwrap();let asrc=a.chart_sources.as_ref().unwrap();let bsrc=b.chart_sources.as_ref().unwrap();
            let mut shared=ai.iter().enumerate().filter_map(|(u,id)|bi.iter().position(|n|n==id).map(|v|(*id,u,v))).collect::<Vec<_>>();shared.sort_by_key(|v|v.0);
            if shared.len()!=2 || !shared.iter().all(|(_,u,v)|asrc[*u].same(&bsrc[*v])){continue;}
            let mut ids=ai.into_iter().chain(bi).collect::<Vec<_>>();ids.sort_unstable();ids.dedup();if ids.len()!=4{continue;}
            let mut controls=Vec::new();let mut original_sources=Vec::new();let mut valid=true;
            for id in &ids {let source=if let Some(u)=ai.iter().position(|n|n==id){&asrc[u]}else{&bsrc[bi.iter().position(|n|n==id).unwrap()]};match source.point_in_frame(anchor){Ok(point)=>{controls.push(point);original_sources.push(source.clone());},Err(_)=>{valid=false;break;}}}
            if !valid{continue;}
            let represented_rank=crate::native_exact_affine_predicate::exact_framed_tetra_sign(std::array::from_fn(|k|controls[k].map(|v|v.value())),[anchor;4]);
            let original_sources:[NativeChartPointSource<S>;4]=original_sources.try_into().map_err(|_|CaeError::contract("cut local chart source rank shape differs"))?;
            let original_rank=crate::native_geometry_construction_error::chart_tetra_expression_rank(&original_sources,anchor);
            if !matches!((represented_rank,original_rank),(Ok(-1),Ok(-1))|(Ok(1),Ok(1))){continue;}
            let sources=[asrc[shared[0].1].clone(),asrc[shared[1].1].clone()];let endpoints=[sources[0].point_in_frame(anchor)?,sources[1].point_in_frame(anchor)?];
            for &(axis,coordinate,cell) in &cells {
                if !same(cell.offset,S::zero()) || !cell.local_origin.is_some_and(|(frame,_)|frame==anchor){continue;}
                let denominator=endpoints[1][axis]-endpoints[0][axis];
                if denominator.value()==0. || !denominator.finite_first_order() || !denominator.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite())){continue;}
                let t=(coordinate-endpoints[0][axis])/denominator;
                if !t.finite_first_order() || !t.geometry_first_order_directions().is_some_and(|d|d.iter().all(|v|v.is_finite())) || !(0.0..=1.0).contains(&t.value()){continue;}
                let mut point=std::array::from_fn(|k|endpoints[0][k]+(endpoints[1][k]-endpoints[0][k])*t);point[axis]=coordinate;
                let Ok(bounds)=crate::native_geometry_construction_error::chart_edge_cell_error(&sources,axis,coordinate,anchor,point)else{continue;};
                let construction=Arc::new(ClipPointConstruction{point,anchor,bounds});
                recipe=Some(OwnedCornerRecipe{point,planes:[incidence_plane(charts[i]),incidence_plane(charts[j]),incidence_plane(cell)],kind:OwnedCornerKind::ChartEdge{region:a.region,ids:[shared[0].0,shared[1].0],sources:sources.clone(),cell:incidence_plane(cell),construction}});break 'chart_edge;
            }
        }}
    }
    if recipe.is_none() && node.construction.bounds(node.point,anchor).is_some() {
        'native_chart_cell:for native in &natives {
            if !same(native.offset,S::zero()){continue;}
            let face=native.native_face.as_ref().unwrap();let Some(source)=&face.source else{continue;};
            if source.validate().is_err(){continue;}
            for chart in &node.incidences {
                let Some(receipt)=&chart.subdomain_face else{continue;};
                if !receipt.valid(&incidence_plane(chart)) || !same(chart.offset,S::zero()) || receipt.native_support.is_some(){continue;}
                let Some(expressions)=&receipt.chart_sources else{continue;};
                for (axis,coordinate,cell) in &cells {
                    if !same(cell.offset,S::zero()) || !cell.local_origin.is_some_and(|(frame,_)|frame==anchor){continue;}
                    let Ok((point,bounds))=crate::native_geometry_construction_error::native_chart_cell_corner(source,face.nodes,expressions,*axis,*coordinate,anchor)else{continue;};
                    if !node.construction.encloses(node.point,anchor,point,anchor) || !(0..3).all(|k|bounds[k].encloses_scalar(node.point[k])){continue;}
                    recipe=Some(OwnedCornerRecipe{point,planes:[incidence_plane(cell),incidence_plane(native),incidence_plane(chart)],kind:OwnedCornerKind::NativeChartCell{construction:Arc::new(ClipPointConstruction{point,anchor,bounds})}});break 'native_chart_cell;
                }
            }
        }
    }
    if recipe.is_none() && node.incidences.len()>=3 && node.incidences.iter().any(|p|p.subdomain_face.is_some()) {
        let mut candidates:Vec<NativeNodeSource<S>>=Vec::new();
        for row in &node.incidences {
            if !native_receipt_qualified(row) || !same(row.offset,S::zero()){continue;}
            let face=row.native_face.as_ref().unwrap();let Some(source)=&face.source else{continue;};
            for id in face.nodes {
                let Some(u)=source.nodes.iter().position(|n|*n==id)else{continue;};let original=&source.points[u];
                if candidates.iter().any(|old|old.same(original)){continue;}
                let Ok(point)=original.point_in_frame(anchor)else{continue;};
                if node.incidences.iter().all(|p|source_vertex_owns_incidence(original,p,anchor)){candidates.push(original.clone());}
            }
        }
        if candidates.len()==1 {
            let source=candidates.pop().unwrap();let point=source.point_in_frame(anchor)?;
            if !node.construction.encloses(node.point,anchor,point,anchor) || !source_vertex_original_rank(&source,&node.incidences,anchor){return Ok(None);}
            let bounds=node.construction.bounds(node.point,anchor).unwrap();
            let bounds=[bounds[0].clone().rebind_enclosed_scalar(point[0])?,bounds[1].clone().rebind_enclosed_scalar(point[1])?,bounds[2].clone().rebind_enclosed_scalar(point[2])?];
            recipe=Some(OwnedCornerRecipe{point,planes:std::array::from_fn(|i|incidence_plane(&node.incidences[i])),kind:OwnedCornerKind::SourceVertex{source,construction:Arc::new(ClipPointConstruction{point,anchor,bounds})}});
        }
    }
    let Some(mut recipe)=recipe else{return Ok(None);};
    'vertex:for native in &natives {
        if matches!(recipe.kind,OwnedCornerKind::SourceVertex{..}){break 'vertex;}
        let r=native.native_face.as_ref().unwrap();
        for (i,id) in r.nodes.iter().enumerate(){
            let point=r.vertices[i];
            if node.incidences.iter().all(|p|if static_cell_coordinate(p,anchor).is_some(){vertex_satisfies_cell(p,point,r.anchor,anchor)}else{native_owns_vertex(p,r.body,*id,point,r.anchor) || levelset_owns_vertex(p,r.body,*id,point,r.anchor) || subdomain_owns_vertex(p,point,r.anchor)}){
                recipe.point=std::array::from_fn(|k|point[k]+(r.anchor[k]-anchor[k]));
                recipe.kind=OwnedCornerKind::Vertex {body:r.body,node:*id,point,anchor:r.anchor};break 'vertex;
            }
        }
    }
    if recipe.point.iter().any(|v|!v.finite_first_order() || !v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite()))){return Err(CaeError::contract("cut local corner solve has incomplete channels"));}
    Ok(Some(recipe))
}
fn recipe_owns_native<S:TransientGasScalar>(recipe:&OwnedCornerRecipe<S>,extra:&CutPlaneIncidence<S>)->bool {
    match &recipe.kind {
        OwnedCornerKind::SourceVertex{source,..}=>source_vertex_owns_incidence(source,extra,[0.;3]),
        OwnedCornerKind::Vertex{body,node,point,anchor}=>native_owns_vertex(extra,*body,*node,*point,*anchor),
        OwnedCornerKind::Edge{body,nodes,points,anchor,..}|OwnedCornerKind::LevelsetEdge{body,nodes,points,anchor,..}=>(0..2).all(|i|native_owns_vertex(extra,*body,nodes[i],points[i],*anchor)),
        OwnedCornerKind::LevelsetFaceCell{body,nodes,points,anchor,..}=>(0..3).all(|i|native_owns_vertex(extra,*body,nodes[i],points[i],*anchor)),
        OwnedCornerKind::ChartVertex{source,..}=>SubdomainNativeSupport::from_plane(&incidence_plane(extra)).is_some_and(|p|p.owns_chart_point(source)),
        OwnedCornerKind::NativeChartCell{..}=>recipe.planes.iter().any(|p|extra.matches(p)),
        OwnedCornerKind::ChartEdge{..}|OwnedCornerKind::Planes=>false,
    }
}
fn recipe_owns_levelset<S:TransientGasScalar>(recipe:&OwnedCornerRecipe<S>,extra:&CutPlaneIncidence<S>)->bool {
    let Some(d)=&extra.native_levelset else{return false;};if !d.valid(&incidence_plane(extra)){return false;}
    match &recipe.kind {
        OwnedCornerKind::SourceVertex{source,..}=>source_vertex_owns_incidence(source,extra,[0.;3]),
        OwnedCornerKind::ChartVertex{source,..}=>SubdomainNativeSupport::from_plane(&incidence_plane(extra)).is_some_and(|p|p.owns_chart_point(source)),
        OwnedCornerKind::LevelsetEdge{body,nodes,points,anchor,values,iso}=>d.body==*body && d.iso==*iso && (0..2).all(|i|d.nodes.iter().position(|id|*id==nodes[i]).is_some_and(|j|same(d.values[j],values[i]) && if let Some(source)=&d.source {source.points[j].point_in_frame(*anchor).is_ok_and(|p|point_same(p,points[i]))}else{framed_point_same(d.vertices[j],d.anchor,points[i],*anchor)})),
        OwnedCornerKind::LevelsetFaceCell{body,nodes,points,anchor,values,iso,..}=>d.body==*body && d.iso==*iso && (0..3).all(|i|d.nodes.iter().position(|id|*id==nodes[i]).is_some_and(|j|same(d.values[j],values[i]) && if let Some(source)=&d.source {source.points[j].point_in_frame(*anchor).is_ok_and(|p|point_same(p,points[i]))}else{framed_point_same(d.vertices[j],d.anchor,points[i],*anchor)})),
        OwnedCornerKind::Vertex{body,node,point,anchor}=>d.body==*body && d.nodes.iter().position(|id|*id==*node).is_some_and(|j|same(d.values[j],S::from_f64(d.iso)) && if let Some(source)=&d.source {source.points[j].point_in_frame(*anchor).is_ok_and(|p|point_same(p,*point))}else{framed_point_same(d.vertices[j],d.anchor,*point,*anchor)}),
        _=>false,
    }
}
fn original_vertex_source<S:TransientGasScalar>(recipe:&OwnedCornerRecipe<S>)->Option<NativeNodeSource<S>> {
    original_vertex_source_from_rows(recipe,&recipe.planes.each_ref().map(|p|CutPlaneIncidence::from_plane(p)))
}
fn original_vertex_source_from_rows<S:TransientGasScalar>(recipe:&OwnedCornerRecipe<S>,rows:&[CutPlaneIncidence<S>])->Option<NativeNodeSource<S>> {
    let OwnedCornerKind::Vertex{body,node,point,anchor}=&recipe.kind else{return None;};
    let mut original:Option<NativeNodeSource<S>>=None;
    for row in rows {
        let Some(face)=&row.native_face else{continue;};
        if face.body!=*body || !face.nodes.contains(node){continue;}
        if !native_receipt_qualified(row) || !same(row.offset,S::zero()){return None;}
        let Some(source)=&face.source else{if !native_owns_vertex(row,*body,*node,*point,*anchor){return None;}continue;};
        if source.validate().is_err(){return None;}
        let u=source.nodes.iter().position(|id|id==node)?;
        let candidate=&source.points[u];
        if candidate.body!=*body || candidate.node!=*node || !candidate.point_in_frame(*anchor).is_ok_and(|p|point_same(p,*point)){return None;}
        if original.as_ref().is_some_and(|old|!old.same(candidate)){return None;}
        original=Some(candidate.clone());
    }
    original
}
fn retained_vertex_sources_agree<S:TransientGasScalar>(recipe:&OwnedCornerRecipe<S>,source:&NativeNodeSource<S>,rows:&[CutPlaneIncidence<S>])->bool {
    let OwnedCornerKind::Vertex{body,node,point,anchor}=&recipe.kind else{return false;};
    rows.iter().all(|row|{
        let Some(face)=&row.native_face else{return true;};
        if face.body!=*body || !face.nodes.contains(node){return true;}
        native_receipt_qualified(row) && same(row.offset,S::zero()) && face.source.as_ref().map_or_else(||native_owns_vertex(row,*body,*node,*point,*anchor),|owner|owner.validate().is_ok() && owner.nodes.iter().position(|id|id==node).is_some_and(|u|owner.points[u].same(source) && owner.points[u].point_in_frame(*anchor).is_ok_and(|p|point_same(p,*point))))
    })
}
fn recipe_owns_subdomain<S:TransientGasScalar>(recipe:&OwnedCornerRecipe<S>,extra:&CutPlaneIncidence<S>)->bool {
    if matches!(recipe.kind,OwnedCornerKind::NativeChartCell{..}){return recipe.planes.iter().any(|p|p.subdomain_face.is_some() && extra.matches(p)) || native_chart_cell_in_subdomain(recipe,&incidence_plane(extra));}
    if let OwnedCornerKind::SourceVertex{source,..}=&recipe.kind{return source_vertex_in_subdomain(source,&incidence_plane(extra));}
    if let Some(source)=original_vertex_source(recipe){return source_vertex_in_subdomain(&source,&incidence_plane(extra));}
    if let OwnedCornerKind::ChartVertex{region,id,source,..}=&recipe.kind {
        if !subdomain_receipt_qualified(extra){return false;}let receipt=extra.subdomain_face.as_ref().unwrap();
        return receipt.region==*region && receipt.vertex_ids.as_ref().zip(receipt.chart_sources.as_ref()).is_some_and(|(ids,sources)|ids.iter().position(|n|n==id).is_some_and(|u|sources[u].same(source)));
    }
    let OwnedCornerKind::ChartEdge{region,ids,sources,..}=&recipe.kind else{return false;};
    if !subdomain_receipt_qualified(extra) || !same(extra.offset,S::zero()){return false;}
    let receipt=extra.subdomain_face.as_ref().unwrap();let (Some(vertices),Some(expressions))=(receipt.vertex_ids,receipt.chart_sources.as_ref())else{return false;};
    receipt.region==*region && (0..2).all(|i|vertices.iter().position(|id|*id==ids[i]).is_some_and(|j|expressions[j].same(&sources[i])))
}
fn source_backed_two_cell_native_corner<S:TransientGasScalar>(recipe:&OwnedCornerRecipe<S>,anchor:[f64;3])->bool {
    if !matches!(recipe.kind,OwnedCornerKind::Planes){return false;}
    let rows=recipe.planes.each_ref().map(|p|CutPlaneIncidence::from_plane(p));
    let (Some((a,x)),Some((b,y)))=(static_cell_coordinate(&rows[0],anchor),static_cell_coordinate(&rows[1],anchor))else{return false;};
    if a==b || !same(recipe.point[a],x) || !same(recipe.point[b],y) || !native_receipt_qualified(&rows[2]) || !same(rows[2].offset,S::zero()){return false;}
    let Some(source)=rows[2].native_face.as_ref().and_then(|r|r.source.as_ref())else{return false;};
    source.validate().is_ok() && recipe.planes.iter().all(|p|qualify_corner_view(recipe.point,p,anchor).is_ok())
}
fn source_backed_native_edge_corner<S:TransientGasScalar>(recipe:&OwnedCornerRecipe<S>,anchor:[f64;3])->bool {
    let OwnedCornerKind::Edge{cell,nodes,..}=&recipe.kind else{return false;};
    let rows=recipe.planes.each_ref().map(|p|CutPlaneIncidence::from_plane(p));
    let Some((axis,coordinate))=static_cell_coordinate(&rows[0],anchor)else{return false;};
    if !rows[0].matches(cell) || !same(recipe.point[axis],coordinate){return false;}
    if !rows[1..].iter().all(|row|native_receipt_qualified(row) && same(row.offset,S::zero()) && recipe_owns_native(recipe,row)
        && row.native_face.as_ref().and_then(|r|r.source.as_ref()).is_some_and(|source|source.validate().is_ok())){return false;}
    let a=rows[1].native_face.as_ref().unwrap().source.as_ref().unwrap();
    let b=rows[2].native_face.as_ref().unwrap().source.as_ref().unwrap();
    if !nodes.iter().all(|id|{let (Some(u),Some(v))=(a.nodes.iter().position(|n|n==id),b.nodes.iter().position(|n|n==id))else{return false;};a.points[u].same(&b.points[v])}){return false;}
    recipe.planes.iter().all(|p|qualify_corner_view(recipe.point,p,anchor).is_ok())
}
fn native_chart_cell_owners_consistent<S:TransientGasScalar>(recipe:&OwnedCornerRecipe<S>,rows:&[CutPlaneIncidence<S>])->bool {
    if !matches!(recipe.kind,OwnedCornerKind::NativeChartCell{..}){return false;}
    let native=recipe.planes[1].native_face.as_ref().unwrap();
    rows.iter().all(|row|{
        if row.native_face.as_ref().is_some_and(|face|face.body==native.body && face.nodes==native.nodes){return row.matches(&recipe.planes[1]);}
        if row.subdomain_face.is_some() && row.boundary==recipe.planes[2].boundary{return row.matches(&recipe.planes[2]);}
        true
    })
}
fn qualify_corner_recipe<S:TransientGasScalar>(node:&LocalNode<S>,recipe:&OwnedCornerRecipe<S>,anchor:[f64;3])->CaeResult<()> {
    if let OwnedCornerKind::NativeChartCell{construction}=&recipe.kind {if !native_chart_cell_owners_consistent(recipe,&node.incidences) || construction.anchor!=anchor || !node.construction.encloses(node.point,anchor,recipe.point,anchor) || !(0..3).all(|k|construction.bounds[k].matches(recipe.point[k]) && construction.bounds[k].encloses_scalar(node.point[k])){return Err(CaeError::contract("cut local native chart corner lacks original construction qualification"));}}
    if source_backed_levelset_edge_corner(recipe,anchor) && !retained_levelset_edge_sources_agree(recipe,&node.incidences){return Err(CaeError::contract("cut local levelset edge has inconsistent source identity"));}
    let original_vertex=original_vertex_source_from_rows(recipe,&node.incidences).or_else(||original_vertex_source(recipe)).or_else(||node.incidences.iter().find_map(|row|original_vertex_source_from_rows(recipe,std::slice::from_ref(row))));
    if let Some(source)=&original_vertex{if !retained_vertex_sources_agree(recipe,source,&node.incidences){return Err(CaeError::contract("cut local original vertex has inconsistent source identity"));}}
    if let OwnedCornerKind::SourceVertex{source,..}=&recipe.kind {if !node.incidences.iter().all(|row|source_vertex_owns_incidence(source,row,anchor)){return Err(CaeError::contract("cut local source vertex has unresolved original ownership"));}}
    if let OwnedCornerKind::ChartVertex{construction,..}=&recipe.kind {if construction.anchor!=anchor || !(0..3).all(|k|construction.bounds[k].matches(recipe.point[k]) && construction.bounds[k].encloses_scalar(node.point[k])){return Err(CaeError::contract("cut local chart vertex differs from original expression"));}}
    if matches!(recipe.kind,OwnedCornerKind::ChartEdge{..}) && !node.construction.encloses(node.point,anchor,recipe.point,anchor){return Err(CaeError::contract("cut local chart corner lacks original construction qualification"));}
    for extra in &node.incidences {
        if let Some((axis,coordinate))=static_cell_coordinate(extra,anchor){
            if !same(recipe.point[axis],coordinate){return Err(CaeError::contract("cut local corner has contradictory cell ownership"));}
        } else if native_receipt_qualified(extra)
            && (recipe.planes.iter().any(|p|extra.matches(p)) || authentic_native_point_incidence(extra,recipe.point,anchor) || recipe_owns_native(recipe,extra)) {
        } else if recipe_owns_levelset(recipe,extra) {
        } else if original_vertex.as_ref().is_some_and(|source|source_vertex_in_subdomain(source,&incidence_plane(extra))) || recipe_owns_subdomain(recipe,extra) || authentic_subdomain_point_incidence(extra,recipe.point,anchor)
            || ((source_backed_native_edge_corner(recipe,anchor) || source_backed_levelset_edge_corner(recipe,anchor)) && original_native_edge_in_subdomain(recipe,&incidence_plane(extra)))
            || recipe.planes.iter().any(|p|subdomain_native_plane_equivalence(extra,p)) {
        }else{return Err(CaeError::contract("cut local corner has unresolved additional ownership"));}
    }
    if let OwnedCornerKind::SourceVertex{construction,..}=&recipe.kind {
        if construction.anchor!=anchor || !(0..3).all(|k|construction.bounds[k].matches(recipe.point[k])) || !node.construction.encloses(node.point,anchor,recipe.point,anchor){return Err(CaeError::contract("cut local source vertex lacks original construction qualification"));}
    }else if matches!(recipe.kind,OwnedCornerKind::ChartVertex{..}|OwnedCornerKind::NativeChartCell{..}) {
    } else if let OwnedCornerKind::Vertex{point,anchor:frame,..}=&recipe.kind {
        if qualify_original_vertex_view(node.point,anchor,*point,*frame).is_err() && !node.construction.encloses(node.point,anchor,*point,*frame){return Err(CaeError::contract("cut local corner view differs from original vertex"));}
    } else {
        for plane in &recipe.planes {
            if qualify_corner_view(node.point,plane,anchor).is_err() && !((matches!(recipe.kind,OwnedCornerKind::LevelsetEdge{..}|OwnedCornerKind::LevelsetFaceCell{..}|OwnedCornerKind::ChartEdge{..}) || source_backed_two_cell_native_corner(recipe,anchor) || source_backed_native_edge_corner(recipe,anchor)) && node.construction.encloses(node.point,anchor,recipe.point,anchor)){return Err(CaeError::contract("cut local corner view differs from original constraints"));}
        }
    }
    Ok(())
}
fn canonicalize_owned_corner<S:TransientGasScalar>(node:&mut LocalNode<S>,anchor:[f64;3])->CaeResult<()> {
    let Some(recipe)=owned_corner_recipe(node,anchor)? else{return Ok(());};
    qualify_corner_recipe(node,&recipe,anchor)?;
    if (source_backed_two_cell_native_corner(&recipe,anchor) || source_backed_native_edge_corner(&recipe,anchor)) && node.construction.encloses(node.point,anchor,recipe.point,anchor) {
        if let Some(bounds)=node.construction.bounds(node.point,anchor) {
            let bounds=[bounds[0].clone().rebind_enclosed_scalar(recipe.point[0])?,bounds[1].clone().rebind_enclosed_scalar(recipe.point[1])?,bounds[2].clone().rebind_enclosed_scalar(recipe.point[2])?];
            node.construction=PointConstruction::Bound(Arc::new(ClipPointConstruction{point:recipe.point,anchor,bounds}));
        }else if !point_same(node.point,recipe.point){node.construction=PointConstruction::Unproved;}
    }else if let OwnedCornerKind::SourceVertex{construction,..}|OwnedCornerKind::ChartEdge{construction,..}|OwnedCornerKind::ChartVertex{construction,..}|OwnedCornerKind::NativeChartCell{construction}=&recipe.kind {
        node.construction=PointConstruction::Bound(construction.clone());
    }else if matches!(recipe.kind,OwnedCornerKind::Vertex{..}){node.construction=PointConstruction::Leaf;}
    else if !point_same(node.point,recipe.point){node.construction=PointConstruction::Unproved;}
    node.point=recipe.point;Ok(())
}
fn clip_point_construction<S:TransientGasScalar>(a:&LocalNode<S>,b:&LocalNode<S>,plane:&Plane<S>,anchor:[f64;3],da:S,db:S,point:[S;3])->PointConstruction<S> {
    if !a.construction.represented(a.point,anchor) || !b.construction.represented(b.point,anchor){return PointConstruction::Unproved;}
    if let Some((axis,coordinate))=clip_cell_coordinate(plane,anchor) {
        return match crate::native_geometry_construction_error::clip_cell_interpolation_error(axis,coordinate,plane.normal,plane_origin_in_frame(plane,anchor),plane.offset,a.point,a.construction.bounds(a.point,anchor),b.point,b.construction.bounds(b.point,anchor),da,db,point){Ok(bounds)=>PointConstruction::Bound(Arc::new(ClipPointConstruction{point,anchor,bounds})),Err(_)=>PointConstruction::Unproved};
    }
    if let Some(r)=&plane.native_levelset {
        if !r.valid(plane){return PointConstruction::Unproved;}
        if let Some(source)=&r.source {
            return match crate::native_geometry_construction_error::clip_native_source_interpolation_error(source,crate::native_geometry_construction_error::NativeClipSource::Levelset{values:r.values,iso:r.iso},plane.normal,plane_origin_in_frame(plane,anchor),plane.offset,a.point,a.construction.bounds(a.point,anchor),b.point,b.construction.bounds(b.point,anchor),anchor,da,db,point){Ok(bounds)=>PointConstruction::Bound(Arc::new(ClipPointConstruction{point,anchor,bounds})),Err(_)=>PointConstruction::Unproved};
        }
        return match crate::native_geometry_construction_error::clip_levelset_interpolation_error(r.vertices,r.values,r.iso,r.anchor,plane.normal,plane_origin_in_frame(plane,anchor),plane.offset,a.point,a.construction.bounds(a.point,anchor),b.point,b.construction.bounds(b.point,anchor),anchor,da,db,point){Ok(bounds)=>PointConstruction::Bound(Arc::new(ClipPointConstruction{point,anchor,bounds})),Err(_)=>PointConstruction::Unproved};
    }
    if let Some(r)=&plane.subdomain_face {
        if !r.valid(plane){return PointConstruction::Unproved;}
        if let Some(support)=&r.native_support {
            let result=match support {
                SubdomainNativeSupport::Face(face)=>{
                    let Some(source)=&face.source else{return PointConstruction::Unproved;};
                    crate::native_geometry_construction_error::clip_native_source_interpolation_error(source,crate::native_geometry_construction_error::NativeClipSource::Face(face.nodes),plane.normal,plane_origin_in_frame(plane,anchor),plane.offset,a.point,a.construction.bounds(a.point,anchor),b.point,b.construction.bounds(b.point,anchor),anchor,da,db,point)
                }
                SubdomainNativeSupport::Levelset(density)=>{
                    let Some(source)=&density.source else{return PointConstruction::Unproved;};
                    crate::native_geometry_construction_error::clip_native_source_interpolation_error(source,crate::native_geometry_construction_error::NativeClipSource::Levelset{values:density.values,iso:density.iso},plane.normal,plane_origin_in_frame(plane,anchor),plane.offset,a.point,a.construction.bounds(a.point,anchor),b.point,b.construction.bounds(b.point,anchor),anchor,da,db,point)
                }
            };
            return match result {Ok(bounds)=>PointConstruction::Bound(Arc::new(ClipPointConstruction{point,anchor,bounds})),Err(_)=>PointConstruction::Unproved};
        }
    }
    if !same(plane.offset,S::zero()){return PointConstruction::Unproved;}
    let source=if let Some(r)=&plane.native_face {
        if !r.valid(plane){return PointConstruction::Unproved;}
        if let Some(source)=&r.source {
            return match crate::native_geometry_construction_error::clip_native_source_interpolation_error(source,crate::native_geometry_construction_error::NativeClipSource::Face(r.nodes),plane.normal,plane_origin_in_frame(plane,anchor),plane.offset,a.point,a.construction.bounds(a.point,anchor),b.point,b.construction.bounds(b.point,anchor),anchor,da,db,point){Ok(bounds)=>PointConstruction::Bound(Arc::new(ClipPointConstruction{point,anchor,bounds})),Err(_)=>PointConstruction::Unproved};
        }
        Some((r.vertices,r.anchor))
    }else if let Some(r)=&plane.subdomain_face {
        if !r.valid(plane){return PointConstruction::Unproved;}
        Some((r.vertices,r.anchor))
    }else {None};
    let Some((triangle,triangle_anchor))=source else{return PointConstruction::Unproved;};
    if let Some(sources)=plane.subdomain_face.as_ref().and_then(|r|r.chart_sources.as_ref()) {
        return match crate::native_geometry_construction_error::clip_chart_interpolation_error(sources,triangle,triangle_anchor,plane.normal,plane_origin_in_frame(plane,anchor),a.point,a.construction.bounds(a.point,anchor),b.point,b.construction.bounds(b.point,anchor),anchor,da,db,point){Ok(bounds)=>PointConstruction::Bound(Arc::new(ClipPointConstruction{point,anchor,bounds})),Err(_)=>PointConstruction::Unproved};
    }
    match crate::native_geometry_construction_error::clip_interpolation_error(triangle,triangle_anchor,plane.normal,plane_origin_in_frame(plane,anchor),a.point,a.construction.bounds(a.point,anchor),b.point,b.construction.bounds(b.point,anchor),anchor,da,db,point) {
        Ok(bounds)=>PointConstruction::Bound(Arc::new(ClipPointConstruction{point,anchor,bounds})),
        Err(_)=>PointConstruction::Unproved,
    }
}

fn insert<S: TransientGasScalar>(out: &mut Vec<LocalNode<S>>, mut node: LocalNode<S>, anchor: [f64;3]) -> CaeResult<()> {
    canonicalize_owned_corner(&mut node,anchor)?;
    if let Some(previous) = out
        .iter_mut()
        .find(|p| (0..3).all(|k| p.point[k].value() == node.point[k].value()))
    {
        if !point_same(previous.point, node.point) {
            return Err(CaeError::contract(
                "cut local coincident vertices have different channels",
            ));
        }
        for incidence in node.incidences {
            let plane = Plane { native_levelset:incidence.native_levelset.clone(), subdomain_face:incidence.subdomain_face.clone(), native_face: incidence.native_face.clone(),
                normal: incidence.normal,
                origin: incidence.origin,
                offset: incidence.offset,
                boundary: incidence.boundary.clone(),
                local_origin: incidence.local_origin,
            };
            if !previous.incidences.iter().any(|p| p.boundary==plane.boundary && p.matches(&plane)) {
                previous.incidences.push(incidence);
            }
        }
    } else {
        out.push(node);
    }
    Ok(())
}
fn source_backed_levelset_edge_corner<S:TransientGasScalar>(recipe:&OwnedCornerRecipe<S>,anchor:[f64;3])->bool {
    let OwnedCornerKind::LevelsetEdge{body,nodes,points,anchor:frame,values,iso}=&recipe.kind else{return false;};
    let rows=recipe.planes.each_ref().map(|p|CutPlaneIncidence::from_plane(p));
    if !rows[..2].iter().all(|row|native_receipt_qualified(row) && same(row.offset,S::zero()) && recipe_owns_native(recipe,row)){return false;}
    let Some(density)=&rows[2].native_levelset else{return false;};
    if !density.valid(&recipe.planes[2]) || !recipe_owns_levelset(recipe,&rows[2]) || density.body!=*body || density.iso!=*iso{return false;}
    let Some(source)=&density.source else{return false;};if source.validate().is_err(){return false;}
    for i in 0..2 {
        let Some(u)=source.nodes.iter().position(|id|*id==nodes[i])else{return false;};
        if !same(density.values[u],values[i]) || !source.points[u].point_in_frame(*frame).is_ok_and(|p|point_same(p,points[i])){return false;}
        for row in &rows[..2] {
            let face=row.native_face.as_ref().unwrap();let Some(owner)=&face.source else{return false;};
            if owner.validate().is_err() || face.body!=*body || !face.nodes.contains(&nodes[i]){return false;}
            let Some(v)=owner.nodes.iter().position(|id|*id==nodes[i])else{return false;};
            if !owner.points[v].same(&source.points[u]){return false;}
        }
    }
    let denominator=values[1]-values[0];
    if denominator.value()==0. || !denominator.finite_first_order() || !denominator.geometry_first_order_directions().is_some_and(|d|d.iter().all(|v|v.is_finite())){return false;}
    let t=(S::from_f64(*iso)-values[0])/denominator;
    let local:[[S;3];2]=points.map(|p|std::array::from_fn(|k|p[k]+(frame[k]-anchor[k])));
    point_same(recipe.point,std::array::from_fn(|k|local[0][k]+(local[1][k]-local[0][k])*t))
}
fn retained_levelset_edge_sources_agree<S:TransientGasScalar>(recipe:&OwnedCornerRecipe<S>,rows:&[CutPlaneIncidence<S>])->bool {
    let OwnedCornerKind::LevelsetEdge{body,nodes,..}=&recipe.kind else{return false;};
    let Some(source)=recipe.planes[2].native_levelset.as_ref().and_then(|d|d.source.as_ref())else{return false;};
    rows.iter().all(|row|{
        if let Some(face)=&row.native_face {
            if face.body!=*body || !nodes.iter().any(|id|face.nodes.contains(id)){return true;}
            if !native_receipt_qualified(row) || !same(row.offset,S::zero()){return false;}
            let Some(owner)=&face.source else{return true;};if owner.validate().is_err(){return false;}
            nodes.iter().filter(|id|face.nodes.contains(id)).all(|id|{
                let (Some(u),Some(v))=(source.nodes.iter().position(|n|n==id),owner.nodes.iter().position(|n|n==id))else{return false;};source.points[u].same(&owner.points[v])
            })
        }else if let Some(density)=&row.native_levelset {
            if density.body!=*body || !nodes.iter().all(|id|density.nodes.contains(id)){return true;}
            if !recipe_owns_levelset(recipe,row){return false;}
            let Some(owner)=&density.source else{return true;};if owner.validate().is_err(){return false;}
            nodes.iter().all(|id|{let (Some(u),Some(v))=(source.nodes.iter().position(|n|n==id),owner.nodes.iter().position(|n|n==id))else{return false;};source.points[u].same(&owner.points[v])})
        }else{true}
    })
}
fn original_node_difference_same<S:TransientGasScalar>(a:&NativeNodeSource<S>,b:&NativeNodeSource<S>,c:&NativeNodeSource<S>,d:&NativeNodeSource<S>)->bool {
    let nodes=[a,b,c,d];
    if nodes.iter().any(|n|n.reference.validate().is_err()){return false;}
    match (&a.reference,&b.reference,&c.reference,&d.reference) {
        (crate::native_geometry_source::NativeReferencePoint::Stored(p),crate::native_geometry_source::NativeReferencePoint::Stored(q),crate::native_geometry_source::NativeReferencePoint::Stored(r),crate::native_geometry_source::NativeReferencePoint::Stored(t))=>{
            if !(0..3).all(|k|matches!(crate::native_exact_affine_predicate::exact_plane_sign([1.,-1.,-1.],[p[k],q[k],r[k]],[0.;3],t[k]),Ok(0))){return false;}
        }
        (crate::native_geometry_source::NativeReferencePoint::Grid{origin,index,spacing},_,_,_)=>{
            let mut indices=[[0usize;3];4];indices[0]=*index;
            for u in 1..4 {
                let crate::native_geometry_source::NativeReferencePoint::Grid{origin:o,index:i,spacing:h}=&nodes[u].reference else{return false;};
                if o.map(f64::to_bits)!=origin.map(f64::to_bits) || h.to_bits()!=spacing.to_bits(){return false;}
                indices[u]=*i;
            }
            if !(0..3).all(|k|indices[0][k] as i128-indices[1][k] as i128-indices[2][k] as i128+indices[3][k] as i128==0){return false;}
        }
        _=>return false,
    }
    for k in 0..3 {
        let values=nodes.map(|n|n.displacement[k]);
        if values.iter().any(|v|!v.finite_first_order()){return false;}
        let channels=values.map(|v|v.geometry_first_order_directions());
        let Some(directions)=channels[0].as_ref()else{return false;};
        if channels.iter().any(|v|v.as_ref().is_none_or(|v|v.len()!=directions.len() || v.iter().any(|x|!x.is_finite()))){return false;}
        if !matches!(crate::native_exact_affine_predicate::exact_plane_sign([1.,-1.,-1.],[values[0].value(),values[1].value(),values[2].value()],[0.;3],values[3].value()),Ok(0)){return false;}
        for direction in 0..directions.len(){
            if !matches!(crate::native_exact_affine_predicate::exact_plane_sign([1.,-1.,-1.],std::array::from_fn(|u|channels[u].as_ref().unwrap()[direction]),[0.;3],channels[3].as_ref().unwrap()[direction]),Ok(0)){return false;}
        }
    }
    true
}

fn original_node_vectors_parallel<S:TransientGasScalar>(a:&NativeNodeSource<S>,b:&NativeNodeSource<S>,c:&NativeNodeSource<S>,d:&NativeNodeSource<S>)->bool {
    let nodes=[a,b,c,d];
    let mut references=[[0.;3];4];
    for (i,node) in nodes.iter().enumerate(){
        if node.reference.validate().is_err(){return false;}
        let crate::native_geometry_source::NativeReferencePoint::Stored(reference)=&node.reference else{return false;};references[i]=*reference;
    }
    let mut values=Vec::new();
    for node in nodes {let mut axes=Vec::new();for scalar in node.displacement {
        if !scalar.finite_first_order(){return false;}
        let Some(directions)=scalar.geometry_first_order_directions()else{return false;};
        if directions.iter().any(|v|!v.is_finite()){return false;}
        let mut channels=vec![scalar.value()];channels.extend(directions);axes.push(channels);
    }values.push(axes);}
    let width=values[0][0].len();
    if values.iter().flatten().any(|axis|axis.len()!=width){return false;}
    let time=if S::geometry_has_time_channel(){if width<2 || width%2!=0{return false;}Some(width/2)}else{None};
    let terms=|u:usize,v:usize,axis:usize,channel:usize|{let mut out=vec![values[u][axis][channel],-values[v][axis][channel]];if channel==0{out.extend([references[u][axis],-references[v][axis]]);}out};
    let mut nonzero=false;
    for axis in 0..3 {match crate::native_exact_affine_predicate::exact_bilinear_sum_zero(&terms(2,3,axis,0).into_iter().map(|v|(v,1.)).collect::<Vec<_>>()){Ok(false)=>nonzero=true,Ok(true)=>{},Err(_)=>return false}}
    if !nonzero{return false;}
    for channel in 0..width {
        let mut placements=if channel==0{vec![(0,0)]}else{vec![(channel,0),(0,channel)]};
        if let Some(time)=time {if channel>time{let spatial=channel-time;placements.extend([(spatial,time),(time,spatial)]);}}
        for (i,j) in [(0,1),(1,2),(2,0)] {
            let mut products=Vec::new();
            for &(u,v) in &placements {
                for x in terms(0,1,i,u){for y in terms(2,3,j,v){products.push((x,y));}}
                for x in terms(0,1,j,u){for y in terms(2,3,i,v){products.push((-x,y));}}
            }
            if !matches!(crate::native_exact_affine_predicate::exact_bilinear_sum_zero(&products),Ok(true)){return false;}
        }
    }
    true
}
fn native_chart_cell_in_subdomain<S:TransientGasScalar>(recipe:&OwnedCornerRecipe<S>,plane:&Plane<S>)->bool {
    if !matches!(recipe.kind,OwnedCornerKind::NativeChartCell{..}) || !same(plane.offset,S::zero()){return false;}
    let Some(query)=&plane.subdomain_face else{return false;};if !query.valid(plane){return false;}
    let Some(charts)=&query.chart_sources else{return false;};
    let Some(chart_nodes)=charts.iter().map(|chart|{
        let feature=chart.boundary_feature()?;
        let crate::moving_contact::surface_map::SurfaceFeature::Vertex(id)=&feature.feature else{return None;};
        if feature.weights().is_err() || feature.points.len()!=1 || feature.scales.len()!=1 || *id!=feature.points[0].node
            || !feature.scales[0].iter().all(|pair|pair[0].to_bits()==pair[1].to_bits()){return None;}
        Some(&feature.points[0])
    }).collect::<Option<Vec<_>>>()else{return false;};
    let native=&recipe.planes[1];let row=CutPlaneIncidence::from_plane(native);if !native_receipt_qualified(&row) || !same(native.offset,S::zero()){return false;}
    let face=native.native_face.as_ref().unwrap();let Some(source)=&face.source else{return false;};if source.validate().is_err(){return false;}
    let Some(indices)=source.face_indices(face.nodes)else{return false;};
    let originals=indices.map(|i|&source.points[i]);
    if let Some(selected)=&recipe.planes[2].subdomain_face {
        if selected.region==query.region && selected.valid(&recipe.planes[2]) && same(recipe.planes[2].offset,S::zero()) {
            if let Some(selected_charts)=&selected.chart_sources {
                let shared=(0..3).filter(|i|selected_charts.iter().any(|chart|chart.same(&charts[*i]))
                    && originals.iter().any(|original|original.same(chart_nodes[*i]))).collect::<Vec<_>>();
                if shared.len()>=2 && shared.iter().enumerate().any(|(u,i)|shared[u+1..].iter().any(|j|!chart_nodes[*i].same(chart_nodes[*j]))){return true;}
            }
        }
    }
    (0..3).any(|present|(0..3).any(|q|originals[present].same(chart_nodes[q])
        && (0..3).any(|missing|missing!=present && (0..3).any(|other|other!=present && other!=missing
            && (0..3).any(|i|i!=q && (0..3).any(|j|j!=q && j!=i
                && original_node_difference_same(originals[missing],originals[present],chart_nodes[i],chart_nodes[j])
                && original_node_vectors_parallel(originals[other],originals[missing],originals[present],chart_nodes[j])))))))
}
fn original_native_edge_in_subdomain<S:TransientGasScalar>(recipe:&OwnedCornerRecipe<S>,plane:&Plane<S>)->bool {
    let (body,nodes,points,anchor)=match &recipe.kind {OwnedCornerKind::Edge{body,nodes,points,anchor,..}|OwnedCornerKind::LevelsetEdge{body,nodes,points,anchor,..}=>(body,nodes,points,anchor),_=>return false};
    let Some(receipt)=&plane.subdomain_face else{return false;};if !receipt.valid(plane) || (matches!(recipe.kind,OwnedCornerKind::LevelsetEdge{..}) && !same(plane.offset,S::zero())){return false;}
    let Some(charts)=&receipt.chart_sources else{return false;};
    let originals=(0..2).map(|i|recipe.planes.iter().find_map(|p|{
        let row=CutPlaneIncidence::from_plane(p);if !native_receipt_qualified(&row){return None;}
        let face=p.native_face.as_ref()?;if face.body!=*body || !face.nodes.contains(&nodes[i]){return None;}
        let source=face.source.as_ref()?;let u=source.nodes.iter().position(|id|*id==nodes[i])?;
        source.points[u].point_in_frame(*anchor).is_ok_and(|point|point_same(point,points[i])).then_some(&source.points[u])
    })).collect::<Option<Vec<_>>>();
    let Some(originals)=originals else{return false;};
    let chart_nodes=charts.iter().map(|chart|{
        let feature=chart.boundary_feature()?;
        let crate::moving_contact::surface_map::SurfaceFeature::Vertex(id)=&feature.feature else{return None;};
        if feature.weights().is_err() || feature.points.len()!=1 || feature.scales.len()!=1 || *id!=feature.points[0].node
            || !feature.scales[0].iter().all(|pair|pair[0].to_bits()==pair[1].to_bits()){return None;}
        Some(&feature.points[0])
    }).collect::<Vec<_>>();
    if originals.iter().all(|original|chart_nodes.iter().flatten().any(|point|point.same(original))){return true;}
    let Some(chart_nodes)=chart_nodes.into_iter().collect::<Option<Vec<_>>>()else{return false;};
    (0..2).any(|present|chart_nodes.iter().any(|point|point.same(originals[present]))
        && (0..3).any(|i|(0..3).any(|j|i!=j && original_node_difference_same(originals[1-present],originals[present],chart_nodes[i],chart_nodes[j]))))
}

fn owned_distance<S: TransientGasScalar>(
    node: &LocalNode<S>,
    plane: &Plane<S>,
    anchor: [f64; 3],
) -> CaeResult<S> {
    if node.incidences.iter().any(|p| p.matches(plane)) {
        return Ok(S::zero());
    }
    if plane.subdomain_face.is_some() {
        if let Some(recipe)=owned_corner_recipe(node,anchor)? {
            if point_same(node.point,recipe.point) && native_chart_cell_in_subdomain(&recipe,plane){qualify_corner_recipe(node,&recipe,anchor)?;return Ok(S::zero());}
            if (point_same(node.point,recipe.point) && (((!matches!(recipe.kind,OwnedCornerKind::LevelsetEdge{..}) || (source_backed_levelset_edge_corner(&recipe,anchor) && retained_levelset_edge_sources_agree(&recipe,&node.incidences))) && original_native_edge_in_subdomain(&recipe,plane)) || original_vertex_source_from_rows(&recipe,&node.incidences).is_some_and(|source|retained_vertex_sources_agree(&recipe,&source,&node.incidences) && source_vertex_in_subdomain(&source,plane)))) || matches!(&recipe.kind,OwnedCornerKind::SourceVertex{source,..} if source_vertex_in_subdomain(source,plane)) {
                qualify_corner_recipe(node,&recipe,anchor)?;return Ok(S::zero());
            }
        }
    }
    let origin = plane_origin_in_frame(plane, anchor);
    let d = super::dot(plane.normal, super::sub(node.point, origin)) + plane.offset;
    if !d.finite_first_order() || d.geometry_first_order_directions().is_none() {
        return Err(CaeError::contract(
            "cut local distance has incomplete channels",
        ));
    }
    if d.value() == 0.
        && d.geometry_first_order_directions()
            .is_some_and(|v| v.iter().any(|v| *v != 0.))
    {
        return Err(CaeError::contract(
            "cut local vertex crosses an unresolved plane incidence",
        ));
    }
    Ok(d)
}
fn common<S: TransientGasScalar>(a: &LocalNode<S>, b: &LocalNode<S>) -> Vec<CutPlaneIncidence<S>> {
    a.incidences
        .iter()
        .filter(|p| {
            let plane = Plane { native_levelset:p.native_levelset.clone(), subdomain_face:p.subdomain_face.clone(), native_face: p.native_face.clone(),
                normal: p.normal,
                origin: p.origin,
                offset: p.offset,
                boundary: p.boundary.clone(),
                local_origin: p.local_origin,
            };
            b.incidences.iter().any(|q| q.boundary==plane.boundary && q.matches(&plane))
        })
        .cloned()
        .collect()
}
pub(crate) fn seed_vertex_plane<S: TransientGasScalar>(
    face: &mut CutPolygon<S>,
    vertex: usize,
    plane: &Plane<S>,
) -> CaeResult<()> {
    let owns_face=face.boundary==plane.boundary;
    let local = face
        .local_geometry
        .as_mut()
        .ok_or_else(|| CaeError::contract("cut local vertex owner is absent"))?;
    if owns_face {local.plane_incidence=Some(CutPlaneIncidence::from_plane(plane));}
    let row = local
        .incidences
        .get_mut(vertex)
        .ok_or_else(|| CaeError::contract("cut local vertex incidence is absent"))?;
    if !row.iter().any(|p| p.boundary==plane.boundary && p.matches(plane)) {
        row.push(CutPlaneIncidence::from_plane(plane));
    }
    Ok(())
}
fn retained<S: Scalar>(
    mut face: CutPolygon<S>,
    anchor: [f64; 3],
    nodes: Vec<LocalNode<S>>,
    local_origin: [S; 3],
) -> CutPolygon<S> {
    let vertices_m = nodes.iter().map(|p| p.point).collect::<Vec<_>>();
    face.vertices_m = vertices_m
        .iter()
        .map(|p| std::array::from_fn(|k| p[k] + anchor[k]))
        .collect();
    let plane_incidence=face.local_geometry.as_ref().and_then(|g|g.plane_incidence.clone());
    let point_constructions=nodes.iter().map(|p|p.construction.clone()).collect();
    face.local_geometry = Some(CutLocalGeometry {
        point_constructions,
        plane_incidence,
        anchor_m: anchor,
        vertices_m,
        plane_origin_m: local_origin,
        incidences: nodes.into_iter().map(|p| p.incidences).collect(),
    });
    face
}
pub(crate) fn clip<S: TransientGasScalar>(
    faces: &[CutPolygon<S>],
    plane: &Plane<S>,
    anchor: [f64; 3],
) -> CaeResult<Vec<CutPolygon<S>>> {
    let mut out = Vec::new();
    let mut cap = Vec::new();
    let mut has_inside = false;
    let mut has_outside = false;
    for face in faces {
        let local_points = polygon_vertices_in_frame(face, anchor);
        let incidences = face.local_geometry.as_ref().map_or_else(
            || vec![Vec::new(); local_points.len()],
            |g| g.incidences.clone(),
        );
        if incidences.len() != local_points.len() {
            return Err(CaeError::contract("cut local incidence shape differs"));
        }
        if face.support_only {
            let mut face = face.clone();
            bind_polygon_frame(&mut face, anchor)?;
            out.push(face);
            continue;
        }
        let constructions=face.local_geometry.as_ref().map_or_else(||vec![PointConstruction::Leaf;local_points.len()],|g|g.point_constructions.clone());
        if constructions.len()!=local_points.len(){return Err(CaeError::contract("cut local construction shape differs"));}
        let nodes = local_points
            .into_iter()
            .zip(incidences).zip(constructions)
            .map(|((point, incidences),construction)| {
                let mut node=LocalNode {point,incidences,construction};
                canonicalize_owned_corner(&mut node,anchor)?;
                Ok(node)
            })
            .collect::<CaeResult<Vec<_>>>()?;
        let mut polygon = Vec::new();
        for i in 0..nodes.len() {
            let a = &nodes[i];
            let b = &nodes[(i + 1) % nodes.len()];
            let da = owned_distance(a, plane, anchor)?;
            let db = owned_distance(b, plane, anchor)?;
            has_inside |= da.value() > 0.;
            has_outside |= da.value() < 0.;
            if da.value() >= 0. {
                let mut a = a.clone();
                if da.value() == 0. {
                    a.incidences.push(CutPlaneIncidence::from_plane(plane));
                    insert(&mut cap, a.clone(), anchor)?;
                }
                insert(&mut polygon, a, anchor)?;
            }
            if (da.value() >= 0.) != (db.value() >= 0.) {
                let mut node = if same(da, S::zero()) {
                    a.clone()
                } else if same(db, S::zero()) {
                    b.clone()
                } else {
                    let t = da / (da - db);
                    let mut point=std::array::from_fn(|k| a.point[k] + (b.point[k] - a.point[k]) * t);
                    if let Some((axis,coordinate))=clip_cell_coordinate(plane,anchor){point[axis]=coordinate;}
                    LocalNode {
                        construction:clip_point_construction(a,b,plane,anchor,da,db,point),
                        point,
                        incidences: common(a, b),
                    }
                };
                node.incidences.push(CutPlaneIncidence::from_plane(plane));
                insert(&mut polygon, node.clone(), anchor)?;
                insert(&mut cap, node, anchor)?;
            }
        }
        let positive = positive_nodes(&polygon)?;
        let mut emitted = face.clone();
        emitted.support_only = !positive;
        if !positive {
            polygon.clear();
        }
        out.push(retained(
            emitted,
            anchor,
            polygon,
            polygon_plane_origin_in_frame(face, anchor),
        ));
    }
    let mut cap_added = false;
    if has_inside && has_outside && cap.len() >= 3 {
        let centre: [f64; 3] = std::array::from_fn(|k| {
            cap.iter().map(|p| p.point[k].value()).sum::<f64>() / cap.len() as f64
        });
        let n = plane.normal.map(|v| v.value());
        let axis = if n[0].abs() < 0.8 {
            [1., 0., 0.]
        } else {
            [0., 1., 0.]
        };
        let u = super::cross(n, axis);
        let u = u.map(|v| v / super::norm(u));
        let v = super::cross(n, u);
        cap.sort_by(|a, b| {
            let a = super::sub(a.point.map(|v| v.value()), centre);
            let b = super::sub(b.point.map(|v| v.value()), centre);
            super::dot(a, v)
                .atan2(super::dot(a, u))
                .total_cmp(&super::dot(b, v).atan2(super::dot(b, u)))
        });
        cap_added = positive_nodes(&cap)?;
    }
    let face = CutPolygon {
        support_only: !cap_added,
        vertices_m: Vec::new(),
        outward_normal: plane.normal.map(|v| -v),
        plane_origin_m: plane.origin,
        plane_offset_m: -plane.offset,
        boundary: plane.boundary.clone(),
        local_geometry: None,
    };
    if !cap_added {
        cap.clear();
    }
    let mut cap_face=retained(face,anchor,cap,plane_origin_in_frame(plane,anchor));
    if let Some(local)=cap_face.local_geometry.as_mut(){local.plane_incidence=Some(CutPlaneIncidence::from_plane(plane));}
    out.push(cap_face);
    Ok(out)
}
pub(crate) fn view_in_frame<S: Scalar>(face: &CutPolygon<S>, anchor: [f64; 3]) -> CutPolygon<S> {
    let mut face = face.clone();
    face.vertices_m = polygon_vertices_in_frame(&face, anchor);
    face.plane_origin_m = polygon_plane_origin_in_frame(&face, anchor);
    face.local_geometry = None;
    face
}

pub(crate) fn plane_origin_in_frame<S: TransientGasScalar>(plane: &Plane<S>, anchor: [f64; 3]) -> [S; 3] {
    if let Some(receipt)=&plane.subdomain_face {if let Some(support)=&receipt.native_support {return if receipt.valid(plane){support.plane().map_or([S::from_f64(f64::NAN);3],|p|plane_origin_in_frame(&p,anchor))}else{[S::from_f64(f64::NAN);3]};}}
    if let Some(receipt)=&plane.native_face {
        if let Some(source)=&receipt.source {
            if receipt.valid(plane) {
                if let Some(indices)=source.face_indices(receipt.nodes) {
                    if let Ok(point)=source.points[indices[0]].point_in_frame(anchor) {return point;}
                }
            }
            return [S::from_f64(f64::NAN);3];
        }
    }
    if let Some(receipt)=&plane.native_levelset {
        if let Some(source)=&receipt.source {
            if receipt.valid(plane) {
                if let Ok(point)=source.points[0].point_in_frame(anchor) {return point;}
            }
            return [S::from_f64(f64::NAN);3];
        }
    }
    match plane.local_origin {
        Some((other, p)) => std::array::from_fn(|k| p[k] + (other[k] - anchor[k])),
        None => std::array::from_fn(|k| plane.origin[k] - anchor[k]),
    }
}

pub(crate) fn exact_same_support<S: TransientGasScalar>(
    a: &CutPolygon<S>,
    b: &CutPolygon<S>,
    _anchor: [f64; 3],
) -> bool {
    let equal = (0..3).all(|k| same(a.outward_normal[k], b.outward_normal[k]));
    let opposite = (0..3).all(|k| same(a.outward_normal[k], -b.outward_normal[k]));
    if !equal && !opposite {
        return false;
    }
    let offset = if opposite {
        -b.plane_offset_m
    } else {
        b.plane_offset_m
    };
    if !same(a.plane_offset_m, offset) {
        return false;
    }
    let (aa, ao) = a.local_geometry.as_ref().map_or(
        ([0.; 3], a.plane_origin_m),
        |g| (g.anchor_m, g.plane_origin_m),
    );
    let (ba, bo) = b.local_geometry.as_ref().map_or(
        ([0.; 3], b.plane_origin_m),
        |g| (g.anchor_m, g.plane_origin_m),
    );
    maintained_plane_origins(a.outward_normal, bo, ba, ao, aa)
}

pub(crate) fn maintained_native_plane_rank_zero<S: TransientGasScalar>(faces:&[CutPolygon<S>])->CaeResult<bool>{
    let mut rows=Vec::new();
    for face in faces.iter().filter(|f|!f.support_only){
        let Some(local)=&face.local_geometry else{return Ok(false);};
        if local.vertices_m.len()!=local.incidences.len(){return Err(CaeError::contract("cut local rank incidence shape differs"));}
        rows.extend(local.incidences.iter());
    }
    let Some(first)=rows.first() else{return Ok(false);};
    for incidence in *first {
        let Some(receipt)=&incidence.native_face else{continue;};
        let plane=Plane{ native_levelset:incidence.native_levelset.clone(), subdomain_face:incidence.subdomain_face.clone(),native_face:incidence.native_face.clone(),boundary:incidence.boundary.clone(),normal:incidence.normal,origin:incidence.origin,offset:incidence.offset,local_origin:incidence.local_origin};
        if !plane.normal.iter().any(|v|v.value()!=0.) || plane.normal.iter().any(|v|!v.finite_first_order() || !v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|v|v.is_finite()))) || !receipt.valid(&plane){continue;}
        if rows.iter().all(|row|row.iter().any(|other|{
            let Some(other_receipt)=&other.native_face else{return false;};
            let other_plane=Plane{ native_levelset:other.native_levelset.clone(), subdomain_face:other.subdomain_face.clone(),native_face:other.native_face.clone(),boundary:other.boundary.clone(),normal:other.normal,origin:other.origin,offset:other.offset,local_origin:other.local_origin};
            receipt.agrees(&plane,other_receipt,&other_plane)
        })){return Ok(true);}
    }
    Ok(false)
}

pub(crate) enum OwnedEdgeOutcome<S:Scalar> {Plane(Plane<S>),Zero,Unproved}
fn same_corner_recipe<S:TransientGasScalar>(a:&OwnedCornerRecipe<S>,b:&OwnedCornerRecipe<S>)->bool {
    match (&a.kind,&b.kind) {
        (OwnedCornerKind::SourceVertex{source:a,..},OwnedCornerKind::SourceVertex{source:b,..})=>a.same(b),
        (OwnedCornerKind::NativeChartCell{..},OwnedCornerKind::NativeChartCell{..})=>a.planes.iter().zip(&b.planes).all(|(x,y)|x.boundary==y.boundary && CutPlaneIncidence::from_plane(x).matches(y)),
        (OwnedCornerKind::Vertex{body:ab,node:an,point:ap,anchor:aa},OwnedCornerKind::Vertex{body:bb,node:bn,point:bp,anchor:ba})=>ab==bb && an==bn && framed_point_same(*ap,*aa,*bp,*ba),
        (OwnedCornerKind::Edge{body:ab,nodes:an,points:ap,anchor:aa,cell:ac},OwnedCornerKind::Edge{body:bb,nodes:bn,points:bp,anchor:ba,cell:bc})=>ab==bb && an==bn && ac.boundary==bc.boundary && CutPlaneIncidence::from_plane(ac).matches(bc) && (0..2).all(|i|framed_point_same(ap[i],*aa,bp[i],*ba)),
        (OwnedCornerKind::LevelsetEdge{body:ab,nodes:an,points:ap,anchor:aa,values:av,iso:ai},OwnedCornerKind::LevelsetEdge{body:bb,nodes:bn,points:bp,anchor:ba,values:bv,iso:bi})=>ab==bb && an==bn && ai==bi && (0..2).all(|i|framed_point_same(ap[i],*aa,bp[i],*ba) && same(av[i],bv[i])),
        (OwnedCornerKind::LevelsetFaceCell{body:ab,nodes:an,points:ap,anchor:aa,values:av,iso:ai,cell:ac},OwnedCornerKind::LevelsetFaceCell{body:bb,nodes:bn,points:bp,anchor:ba,values:bv,iso:bi,cell:bc})=>ab==bb && an==bn && ai==bi && ac.boundary==bc.boundary && CutPlaneIncidence::from_plane(ac).matches(bc) && (0..3).all(|i|framed_point_same(ap[i],*aa,bp[i],*ba) && same(av[i],bv[i])),
        (OwnedCornerKind::ChartVertex{region:ar,id:ai,source:asrc,..},OwnedCornerKind::ChartVertex{region:br,id:bi,source:bsrc,..})=>ar==br && ai==bi && asrc.same(bsrc),
        (OwnedCornerKind::ChartEdge{region:ar,ids:ai,sources:asrc,cell:ac,..},OwnedCornerKind::ChartEdge{region:br,ids:bi,sources:bsrc,cell:bc,..})=>ar==br && ai==bi && (0..2).all(|i|asrc[i].same(&bsrc[i])) && ac.boundary==bc.boundary && CutPlaneIncidence::from_plane(ac).matches(bc),
        (OwnedCornerKind::Planes,OwnedCornerKind::Planes)=>(0..3).all(|i|a.planes[i].boundary==b.planes[i].boundary && CutPlaneIncidence::from_plane(&a.planes[i]).matches(&b.planes[i])),
        _=>false,
    }
}
pub(crate) fn authentic_edge_source<S:TransientGasScalar>(face:&CutPolygon<S>,anchor:[f64;3])->CaeResult<bool> {
    let Some(local)=&face.local_geometry else{return Ok(false);};
    if local.vertices_m.len()!=local.incidences.len(){return Err(CaeError::contract("cut local edge owner shape differs"));}
    let Some(source)=&local.plane_incidence else{return Ok(false);};
    let source_plane=incidence_plane(source);
    if source.boundary!=face.boundary {return Ok(false);}
    let orientation=if (0..3).all(|k|same(face.outward_normal[k],source.normal[k])) {1.}
        else if (0..3).all(|k|same(face.outward_normal[k],-source.normal[k])) {-1.}
        else{return Ok(false);};
    if !same(face.plane_offset_m,source.offset*orientation){return Ok(false);}
    if !local.incidences.iter().all(|row|row.iter().any(|p|p.boundary==source.boundary && p.matches(&source_plane))){return Ok(false);}
    let points=polygon_vertices_in_frame(face,anchor);
    if points.iter().flatten().any(|v|!v.finite_first_order() || !v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite()))){return Err(CaeError::contract("cut local edge source has incomplete channels"));}
    if let Some((axis,coordinate))=static_cell_coordinate(source,anchor) {
        let actual=Plane { native_levelset:None, subdomain_face:None,native_face:None,normal:face.outward_normal,origin:face.plane_origin_m,offset:face.plane_offset_m,boundary:face.boundary.clone(),local_origin:Some((local.anchor_m,local.plane_origin_m))};
        return Ok(source.matches(&actual) && points.iter().all(|p|same(p[axis],coordinate)));
    }
    if let Some(density)=&source.native_levelset {
        if !density.valid(&source_plane) || native_levelset_for_polygon(face).is_none(){return Ok(false);}
        for (i,point) in points.into_iter().enumerate() {
            let mut node=LocalNode{point,incidences:local.incidences[i].clone(),construction:local.point_constructions.get(i).cloned().unwrap_or(PointConstruction::Unproved)};
            canonicalize_owned_corner(&mut node,anchor)?;
            let Some(recipe)=owned_corner_recipe(&node,anchor)? else{return Ok(false);};
            qualify_corner_recipe(&node,&recipe,anchor)?;
            if !recipe_owns_levelset(&recipe,source){return Ok(false);}
        }
        return Ok(true);
    }
    if !native_receipt_qualified(source) || !same(source.offset,S::zero()) || native_face_for_polygon(face).is_none(){return Ok(false);}
    for point in points {qualify_native_corner_view(point,source,anchor)?;}
    Ok(true)
}
pub(crate) fn classify_owned_edge<S:TransientGasScalar>(face:&CutPolygon<S>,edge_index:usize,anchor:[f64;3])->CaeResult<OwnedEdgeOutcome<S>> {
    let Some(local)=&face.local_geometry else{return Ok(OwnedEdgeOutcome::Unproved);};
    if local.vertices_m.len()!=local.incidences.len() || local.vertices_m.len()!=local.point_constructions.len() || edge_index>=local.vertices_m.len(){return Err(CaeError::contract("cut local edge owner shape differs"));}
    let points=polygon_vertices_in_frame(face,anchor);let end=(edge_index+1)%points.len();
    let nodes=[LocalNode{point:points[edge_index],incidences:local.incidences[edge_index].clone(),construction:local.point_constructions.get(edge_index).cloned().unwrap_or(PointConstruction::Unproved)},LocalNode{point:points[end],incidences:local.incidences[end].clone(),construction:local.point_constructions.get(end).cloned().unwrap_or(PointConstruction::Unproved)}];
    if let (Some(a),Some(b))=(owned_corner_recipe(&nodes[0],anchor)?,owned_corner_recipe(&nodes[1],anchor)?){
        qualify_corner_recipe(&nodes[0],&a,anchor)?;qualify_corner_recipe(&nodes[1],&b,anchor)?;
        if same_corner_recipe(&a,&b) && point_same(a.point,b.point) && authentic_edge_source(face,anchor)? {return Ok(OwnedEdgeOutcome::Zero);}
    }
    if let Some(plane)=original_cell_edge_plane(face,edge_index,anchor)? {return Ok(OwnedEdgeOutcome::Plane(plane));}
    if let Some(plane)=original_native_edge_plane(face,edge_index,anchor)? {return Ok(OwnedEdgeOutcome::Plane(plane));}
    if let Some(plane)=original_native_edge_from_levelset_source(face,edge_index,anchor)? {return Ok(OwnedEdgeOutcome::Plane(plane));}
    Ok(match original_levelset_edge_plane(face,edge_index,anchor)? {Some(plane)=>OwnedEdgeOutcome::Plane(plane),None=>OwnedEdgeOutcome::Unproved})
}
pub(crate) fn original_cell_edge_plane<S:TransientGasScalar>(face:&CutPolygon<S>,edge_index:usize,anchor:[f64;3])->CaeResult<Option<Plane<S>>> {
    let Some(local)=&face.local_geometry else{return Ok(None);};
    if local.vertices_m.len()!=local.incidences.len() || local.vertices_m.len()!=local.point_constructions.len() || edge_index>=local.vertices_m.len(){return Err(CaeError::contract("cut local edge owner shape differs"));}
    let Some(source)=&local.plane_incidence else{return Ok(None);};
    let source_plane=incidence_plane(source);
    if !authentic_edge_source(face,anchor)? {return Ok(None);}
    let points=polygon_vertices_in_frame(face,anchor);
    let end=(edge_index+1)%points.len();
    let mut nodes=[LocalNode{point:points[edge_index],incidences:local.incidences[edge_index].clone(),construction:local.point_constructions.get(edge_index).cloned().unwrap_or(PointConstruction::Unproved)},LocalNode{point:points[end],incidences:local.incidences[end].clone(),construction:local.point_constructions.get(end).cloned().unwrap_or(PointConstruction::Unproved)}];
    for node in &mut nodes {
        canonicalize_owned_corner(node,anchor)?;
        if !node.incidences.iter().any(|p|p.boundary==source.boundary && p.matches(&source_plane)){return Ok(None);}
    }
    let edge=super::sub(nodes[1].point,nodes[0].point);
    let cross=super::cross(face.outward_normal,edge);
    if !cross.iter().any(|v|v.value()!=0.) || cross.iter().any(|v|!v.finite_first_order() || !v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite()))){return Ok(None);}
    let centre:[S;3]=std::array::from_fn(|k|points.iter().map(|p|p[k]).fold(S::zero(),|a,b|a+b)/points.len() as f64);
    for candidate in &nodes[0].incidences {
        let Some((axis,coordinate))=static_cell_coordinate(candidate,anchor) else{continue;};
        let mut plane=incidence_plane(candidate);
        if !nodes[1].incidences.iter().any(|p|p.boundary==candidate.boundary && p.matches(&plane))
            || !nodes.iter().all(|node|same(node.point[axis],coordinate)){continue;}
        let residual=super::dot(plane.normal,super::sub(centre,plane_origin_in_frame(&plane,anchor)))+plane.offset;
        if !residual.finite_first_order() || !residual.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite())){return Err(CaeError::contract("cut local edge orientation has incomplete channels"));}
        if residual.value()==0.{continue;}
        if residual.value()<0. {plane.normal=plane.normal.map(|v|-v);plane.offset=-plane.offset;}
        return Ok(Some(plane));
    }
    Ok(None)
}

fn original_grid_node_in_native_plane<S:TransientGasScalar>(original:&NativeNodeSource<S>,owner:&CutPlaneIncidence<S>,witness:&NativeElementSource<S>)->bool {
    if !native_receipt_qualified(owner) || !same(owner.offset,S::zero()){return false;}
    let face=owner.native_face.as_ref().unwrap();
    let Some(source)=&face.source else{return false;};
    if face.body!=original.body || witness.body!=original.body || source.validate().is_err() || witness.validate().is_err(){return false;}
    if !witness.nodes.iter().position(|id|*id==original.node).is_some_and(|i|witness.points[i].same(original)){return false;}
    let shared:Vec<_>=witness.nodes.iter().filter_map(|id|source.nodes.iter().position(|n|n==id).map(|v|(witness.nodes.iter().position(|n|n==id).unwrap(),v))).collect();
    if shared.is_empty() || !shared.iter().all(|(u,v)|witness.points[*u].same(&source.points[*v])){return false;}
    let crate::native_geometry_source::NativeReferencePoint::Grid{origin,index,spacing}=&original.reference else{return false;};
    let indices=face.nodes.map(|id|source.nodes.iter().position(|n|*n==id));
    if indices.iter().any(Option::is_none){return false;}
    let points=indices.map(|i|&source.points[i.unwrap()]);
    let mut grid=[[0i128;3];3];
    for u in 0..3 {
        let crate::native_geometry_source::NativeReferencePoint::Grid{origin:o,index:i,spacing:h}=&points[u].reference else{return false;};
        if o.map(f64::to_bits)!=origin.map(f64::to_bits) || h.to_bits()!=spacing.to_bits(){return false;}
        grid[u]=i.map(|v|v as i128);
    }
    if original.reference.validate().is_err(){return false;}
    let d1: [i128;3]=std::array::from_fn(|k|grid[1][k]-grid[0][k]);
    let d2: [i128;3]=std::array::from_fn(|k|grid[2][k]-grid[0][k]);
    let r: [i128;3]=std::array::from_fn(|k|index[k] as i128-grid[0][k]);
    for (a,b) in [(0,1),(0,2),(1,2)] {
        let det=d1[a]*d2[b]-d1[b]*d2[a];if det==0{continue;}
        let nu=r[a]*d2[b]-r[b]*d2[a];let nv=d1[a]*r[b]-d1[b]*r[a];
        if nu%det!=0 || nv%det!=0{return false;}
        let u=nu/det;let v=nv/det;let coefficients=[1-u-v,u,v];
        if coefficients.iter().any(|n|n.abs()>(1i128<<53)) || !(0..3).all(|k|r[k]==u*d1[k]+v*d2[k]){return false;}
        let normal=coefficients.map(|n|n as f64);
        for k in 0..3 {
            let values=points.map(|p|p.displacement[k]);let target=original.displacement[k];
            if !matches!(crate::native_exact_affine_predicate::exact_plane_sign(normal,values.map(|s|s.value()),[0.;3],-target.value()),Ok(0)){return false;}
            let Some(directions)=target.geometry_first_order_directions()else{return false;};
            let channels=values.map(|value|value.geometry_first_order_directions());
            if channels.iter().any(|c|c.as_ref().is_none_or(|c|c.len()!=directions.len())){return false;}
            for direction in 0..directions.len(){
                if !matches!(crate::native_exact_affine_predicate::exact_plane_sign(normal,std::array::from_fn(|u|channels[u].as_ref().unwrap()[direction]),[0.;3],-directions[direction]),Ok(0)){return false;}
            }
        }
        return true;
    }
    false
}

fn terminal_recipe_owns_native<S:TransientGasScalar>(node:&LocalNode<S>,recipe:&OwnedCornerRecipe<S>,owner:&CutPlaneIncidence<S>,anchor:[f64;3])->bool {
    if !point_same(node.point,recipe.point){return false;}
    if let OwnedCornerKind::Vertex{body,node:id,point,anchor:frame}=&recipe.kind {
        if !framed_point_same(recipe.point,anchor,*point,*frame){return false;}
        let Some(original)=original_vertex_source_from_rows(recipe,&node.incidences).or_else(||original_vertex_source(recipe))else{return false;};
        if !retained_vertex_sources_agree(recipe,&original,&node.incidences) || !native_receipt_qualified(owner) || !same(owner.offset,S::zero()){return false;}
        let Some(face)=&owner.native_face else{return false;};
        let Some(source)=&face.source else{return false;};
        return face.body==*body && face.nodes.contains(id) && source.validate().is_ok()
            && source.nodes.iter().position(|n|n==id).is_some_and(|i|source.points[i].same(&original))
            && native_owns_vertex(owner,*body,*id,*point,*frame);
    }

    if source_backed_two_cell_native_corner(recipe,anchor) {
        let selected=CutPlaneIncidence::from_plane(&recipe.planes[2]);
        let face=selected.native_face.as_ref().unwrap();
        let source=face.source.as_ref().unwrap();
        let Some(receipt)=owner.native_face.as_ref() else{return false;};
        if !native_receipt_qualified(owner) || !same(owner.offset,S::zero()){return false;}
        let Some(owner_source)=receipt.source.as_ref() else{return false;};
        if owner_source.validate().is_err(){return false;}
        return face.nodes.iter().any(|id|source.nodes.iter().position(|n|n==id).is_some_and(|u|{
            let original=&source.points[u];
            original.point_in_frame(anchor).is_ok_and(|point|point_same(point,recipe.point) && (original_grid_node_in_native_plane(original,owner,source) || node.incidences.iter().any(|row|{
                native_owns_vertex(row,original.body,original.node,point,anchor)
                    && row.native_face.as_ref().is_some_and(|retained|retained.source.as_ref().is_some_and(|retained_source|retained_source.validate().is_ok() && retained_source.nodes.iter().position(|n|*n==original.node).is_some_and(|v|retained_source.points[v].same(original)) && retained.nodes.iter().filter(|id|receipt.nodes.contains(id)).all(|id|retained_source.nodes.iter().position(|n|n==id).zip(owner_source.nodes.iter().position(|n|n==id)).is_some_and(|(v,w)|retained_source.points[v].same(&owner_source.points[w]))))
                        && retained.agrees(&incidence_plane(row),receipt,&incidence_plane(owner)))
            })))
        }));
    }
    let (body,ids,points,frame)=match &recipe.kind {
        OwnedCornerKind::Edge{body,nodes,points,anchor,..}|OwnedCornerKind::LevelsetEdge{body,nodes,points,anchor,..}=>(*body,*nodes,*points,*anchor),
        _=>return false,
    };
    let Some(receipt)=owner.native_face.as_ref() else{return false;};
    (0..2).any(|i|framed_point_same(recipe.point,anchor,points[i],frame) && node.incidences.iter().any(|row|{
        native_owns_vertex(row,body,ids[i],points[i],frame) && row.native_face.as_ref().is_some_and(|r|r.agrees(&incidence_plane(row),receipt,&incidence_plane(owner)))
    }))
}

fn original_native_edge_plane<S:TransientGasScalar>(face:&CutPolygon<S>,edge_index:usize,anchor:[f64;3])->CaeResult<Option<Plane<S>>> {
    if !authentic_edge_source(face,anchor)? {return Ok(None);}
    let local=face.local_geometry.as_ref().unwrap();
    let source=local.plane_incidence.as_ref().unwrap();
    let sr=source.native_face.as_ref();
    let source_cell=static_cell_coordinate(source,anchor);
    let points=polygon_vertices_in_frame(face,anchor);
    let end=(edge_index+1)%points.len();
    let mut nodes=[LocalNode{point:points[edge_index],incidences:local.incidences[edge_index].clone(),construction:local.point_constructions.get(edge_index).cloned().unwrap_or(PointConstruction::Unproved)},LocalNode{point:points[end],incidences:local.incidences[end].clone(),construction:local.point_constructions.get(end).cloned().unwrap_or(PointConstruction::Unproved)}];
    for node in &mut nodes {canonicalize_owned_corner(node,anchor)?;}
    let edge=super::sub(nodes[1].point,nodes[0].point);
    let cross=super::cross(face.outward_normal,edge);
    if !cross.iter().any(|v|v.value()!=0.) || cross.iter().any(|v|!v.finite_first_order() || !v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite()))){return Ok(None);}
    let centre:[S;3]=std::array::from_fn(|k|points.iter().map(|p|p[k]).fold(S::zero(),|a,b|a+b)/points.len() as f64);
    for candidate in &nodes[0].incidences {
        if !native_receipt_qualified(candidate) || !same(candidate.offset,S::zero()){continue;}
        let cr=candidate.native_face.as_ref().unwrap();
        let mut plane=incidence_plane(candidate);
        if !nodes[1].incidences.iter().any(|p|p.boundary==candidate.boundary && p.matches(&plane)){continue;}
        let mut one_shared=false;
        if let Some(sr)=sr {
            if sr.body!=cr.body {continue;}
            let common:Vec<_>=sr.nodes.iter().enumerate().filter_map(|(i,id)|cr.nodes.iter().position(|other|other==id).map(|j|(i,j))).collect();
            if !common.iter().all(|(i,j)|framed_point_same(sr.vertices[*i],sr.anchor,cr.vertices[*j],cr.anchor)){continue;}
            if common.len()==1 {
                if !(0..3).any(|i|matches!(crate::native_exact_affine_predicate::exact_framed_tetra_sign([sr.vertices[0].map(|v|v.value()),sr.vertices[1].map(|v|v.value()),sr.vertices[2].map(|v|v.value()),cr.vertices[i].map(|v|v.value())],[sr.anchor,sr.anchor,sr.anchor,cr.anchor]),Ok(-1)|Ok(1))){continue;}
                one_shared=true;
            } else {
                if common.len()!=2{continue;}
                let si=(0..3).find(|i|!common.iter().any(|(j,_)|i==j)).unwrap();
                let ci=(0..3).find(|i|!common.iter().any(|(_,j)|i==j)).unwrap();
                let primitive=[sr.vertices[common[0].0].map(|v|v.value()),sr.vertices[common[1].0].map(|v|v.value()),sr.vertices[si].map(|v|v.value()),cr.vertices[ci].map(|v|v.value())];
                if !matches!(crate::native_exact_affine_predicate::exact_framed_tetra_sign(primitive,[sr.anchor,sr.anchor,sr.anchor,cr.anchor]),Ok(-1)|Ok(1)){continue;}
            }
        } else if let Some((axis,_))=source_cell {
            let mut normal=[0.;3];normal[axis]=1.;
            if !(1..3).any(|i|matches!(crate::native_exact_affine_predicate::exact_framed_plane_sign(normal,cr.vertices[i].map(|v|v.value()),cr.anchor,cr.vertices[0].map(|v|v.value()),cr.anchor,0.),Ok(-1)|Ok(1))){continue;}
        } else {continue;}
        let mut supported=true;
        for node in &nodes {
            qualify_native_corner_view(node.point,candidate,anchor)?;
            let recipe=owned_corner_recipe(node,anchor)?;
            if one_shared {
                let Some(recipe)=recipe else{supported=false;break;};
                qualify_corner_recipe(node,&recipe,anchor)?;
                let owns=|owner:&CutPlaneIncidence<S>|recipe.planes.iter().any(|p|{let row=CutPlaneIncidence::from_plane(p);native_receipt_qualified(&row) && row.native_face.as_ref().zip(owner.native_face.as_ref()).is_some_and(|(a,b)|a.agrees(p,b,&incidence_plane(owner)))});
                if !(owns(source) || terminal_recipe_owns_native(node,&recipe,source,anchor)) || !(owns(candidate) || terminal_recipe_owns_native(node,&recipe,candidate,anchor)){supported=false;break;}
                continue;
            }
            if !authentic_native_point_incidence(candidate,node.point,anchor)
                && !recipe.as_ref().is_some_and(|r|recipe_owns_native(r,candidate) || (source_cell.is_some() && r.planes.iter().any(|p|candidate.matches(p)))){supported=false;break;}
            if let Some((axis,coordinate))=source_cell {if !same(node.point[axis],coordinate){supported=false;break;}}
            else if !authentic_native_point_incidence(source,node.point,anchor)
                && !recipe.as_ref().is_some_and(|r|recipe_owns_native(r,source)){supported=false;break;}
        }
        if !supported {continue;}
        let residual=super::dot(plane.normal,super::sub(centre,plane_origin_in_frame(&plane,anchor)))+plane.offset;
        if !residual.finite_first_order() || !residual.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite())){return Err(CaeError::contract("cut local edge orientation has incomplete channels"));}
        if residual.value()==0. {continue;}
        if residual.value()<0. {plane.normal=plane.normal.map(|v|-v);plane.offset=-plane.offset;}
        return Ok(Some(plane));
    }
    Ok(None)
}


fn original_native_edge_from_levelset_source<S:TransientGasScalar>(face:&CutPolygon<S>,edge_index:usize,anchor:[f64;3])->CaeResult<Option<Plane<S>>> {
    let Some(local)=&face.local_geometry else{return Ok(None);};
    if local.vertices_m.len()!=local.incidences.len() || local.vertices_m.len()!=local.point_constructions.len() || edge_index>=local.vertices_m.len(){return Err(CaeError::contract("cut local edge owner shape differs"));}
    let Some(source)=&local.plane_incidence else{return Ok(None);};
    let Some(density)=&source.native_levelset else{return Ok(None);};
    if !authentic_edge_source(face,anchor)?{return Ok(None);}
    let points=polygon_vertices_in_frame(face,anchor);
    let end=(edge_index+1)%points.len();
    let mut nodes=[LocalNode{point:points[edge_index],incidences:local.incidences[edge_index].clone(),construction:local.point_constructions[edge_index].clone()},LocalNode{point:points[end],incidences:local.incidences[end].clone(),construction:local.point_constructions[end].clone()}];
    let mut recipes=Vec::new();
    for node in &mut nodes {
        canonicalize_owned_corner(node,anchor)?;
        let Some(recipe)=owned_corner_recipe(node,anchor)? else{return Ok(None);};
        qualify_corner_recipe(node,&recipe,anchor)?;
        if !recipe_owns_levelset(&recipe,source){return Ok(None);}
        recipes.push(recipe);
    }
    let cross=super::cross(face.outward_normal,super::sub(nodes[1].point,nodes[0].point));
    let finite=|v:S|v.finite_first_order() && v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite()));
    if !cross.iter().any(|v|v.value()!=0.) || cross.iter().copied().any(|v|!finite(v)){return Ok(None);}
    let centre:[S;3]=std::array::from_fn(|k|points.iter().map(|p|p[k]).fold(S::zero(),|a,b|a+b)/points.len() as f64);
    for candidate in &nodes[0].incidences {
        if !native_receipt_qualified(candidate) || !same(candidate.offset,S::zero()){continue;}
        let receipt=candidate.native_face.as_ref().unwrap();
        if receipt.body!=density.body{continue;}
        let mut plane=incidence_plane(candidate);
        if !recipes.iter().all(|recipe|recipe_owns_native(recipe,candidate) || recipe.planes.iter().any(|selected|{
            let row=CutPlaneIncidence::from_plane(selected);
            native_receipt_qualified(&row) && same(row.offset,S::zero())
                && row.native_face.as_ref().unwrap().agrees(selected,receipt,&plane)
        })){continue;}
        let indices=receipt.nodes.map(|id|density.nodes.iter().position(|n|*n==id));
        if indices.iter().any(Option::is_none){continue;}
        let indices=indices.map(Option::unwrap);
        let same_controls=match (&receipt.source,&density.source){
            (Some(native_source),Some(density_source))=>(0..3).all(|i|native_source.nodes.iter().position(|id|*id==receipt.nodes[i]).is_some_and(|j|native_source.points[j].same(&density_source.points[indices[i]]))),
            (None,None)=>(0..3).all(|i|framed_point_same(receipt.vertices[i],receipt.anchor,density.vertices[indices[i]],density.anchor)),
            _=>false,
        };
        if !same_controls || !(1..3).any(|i|density.values[indices[i]].value()!=density.values[indices[0]].value()){continue;}
        let residual=super::dot(plane.normal,super::sub(centre,plane_origin_in_frame(&plane,anchor)))+plane.offset;
        if !finite(residual){return Err(CaeError::contract("cut local edge orientation has incomplete channels"));}
        if residual.value()==0.{continue;}
        if residual.value()<0.{plane.normal=plane.normal.map(|v|-v);plane.offset=-plane.offset;}
        return Ok(Some(plane));
    }
    Ok(None)
}

fn original_levelset_edge_plane<S:TransientGasScalar>(face:&CutPolygon<S>,edge_index:usize,anchor:[f64;3])->CaeResult<Option<Plane<S>>> {
    if !authentic_edge_source(face,anchor)? {return Ok(None);}
    let local=face.local_geometry.as_ref().unwrap();
    let source=local.plane_incidence.as_ref().unwrap();
    let source_receipt=source.native_face.as_ref();
    let source_cell=static_cell_coordinate(source,anchor).filter(|_|same(source.offset,S::zero()) && source.native_face.is_none() && source.native_levelset.is_none() && source.subdomain_face.is_none());
    if source_receipt.is_none() && source_cell.is_none(){return Ok(None);}
    let points=polygon_vertices_in_frame(face,anchor);
    let end=(edge_index+1)%points.len();
    let mut nodes=[LocalNode{point:points[edge_index],incidences:local.incidences[edge_index].clone(),construction:local.point_constructions[edge_index].clone()},LocalNode{point:points[end],incidences:local.incidences[end].clone(),construction:local.point_constructions[end].clone()}];
    for node in &mut nodes {canonicalize_owned_corner(node,anchor)?;}
    let mut recipes=Vec::new();
    for node in &nodes {
        let Some(recipe)=owned_corner_recipe(node,anchor)? else{return Ok(None);};
        qualify_corner_recipe(node,&recipe,anchor)?;
        recipes.push(recipe);
    }
    let edge=super::sub(nodes[1].point,nodes[0].point);
    let cross=super::cross(face.outward_normal,edge);
    let finite=|v:S|v.finite_first_order() && v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|x|x.is_finite()));
    if !cross.iter().any(|v|v.value()!=0.) || cross.iter().copied().any(|v|!finite(v)){return Ok(None);}
    let centre:[S;3]=std::array::from_fn(|k|points.iter().map(|p|p[k]).fold(S::zero(),|a,b|a+b)/points.len() as f64);
    for candidate in &nodes[0].incidences {
        let Some(density)=candidate.native_levelset.as_ref() else{continue;};
        let mut plane=incidence_plane(candidate);
        if !density.valid(&plane) || source_receipt.is_some_and(|r|density.body!=r.body) || !recipes.iter().all(|r|recipe_owns_levelset(r,candidate)){continue;}
        if let Some((axis,coordinate))=source_cell {
            if !nodes.iter().all(|n|same(n.point[axis],coordinate)) || !recipes.iter().all(|recipe|match &recipe.kind {
                OwnedCornerKind::LevelsetFaceCell{cell,..}=>CutPlaneIncidence::from_plane(cell).matches(&incidence_plane(source)) && static_cell_coordinate(&CutPlaneIncidence::from_plane(cell),anchor).is_some_and(|(a,c)|a==axis && same(c,coordinate)),
                _=>false,
            }){continue;}
        }else{
        let source_receipt=source_receipt.unwrap();
        let source_supported=recipes.iter().all(|recipe|recipe_owns_native(recipe,source) || recipe.planes.iter().any(|selected|{
            let row=CutPlaneIncidence::from_plane(selected);
            native_receipt_qualified(&row) && same(row.offset,S::zero())
                && row.native_face.as_ref().unwrap().agrees(selected,source_receipt,&incidence_plane(source))
        }));
        if !source_supported {continue;}
        let transverse=|receipt:&NativePlaneFace<S>|{
            let indices=receipt.nodes.map(|id|density.nodes.iter().position(|n|*n==id));
            if indices.iter().any(Option::is_none){return false;}
            let indices=indices.map(Option::unwrap);
            (0..3).all(|i|framed_point_same(receipt.vertices[i],receipt.anchor,density.vertices[indices[i]],density.anchor))
                && (1..3).any(|i|density.values[indices[i]].value()!=density.values[indices[0]].value())
        };
        let supported=transverse(source_receipt) || recipes.iter().any(|recipe|recipe.planes.iter().any(|selected|{
            let row=CutPlaneIncidence::from_plane(selected);
            native_receipt_qualified(&row) && same(row.offset,S::zero())
                && row.native_face.as_ref().unwrap().agrees(selected,source_receipt,&incidence_plane(source))
                && transverse(row.native_face.as_ref().unwrap())
        }));
        if !supported {continue;}
        }
        let residual=super::dot(plane.normal,super::sub(centre,plane_origin_in_frame(&plane,anchor)))+plane.offset;
        if !finite(residual){return Err(CaeError::contract("cut local edge orientation has incomplete channels"));}
        if residual.value()==0. {continue;}
        if residual.value()<0. {plane.normal=plane.normal.map(|v|-v);plane.offset=-plane.offset;}
        return Ok(Some(plane));
    }
    Ok(None)
}

pub(crate) fn unique_volume_vertices<S: TransientGasScalar>(
    faces: &[CutPolygon<S>],
    anchor: [f64; 3],
) -> CaeResult<usize> {
    let mut nodes = Vec::new();
    for face in faces.iter().filter(|f| !f.support_only) {
        let points=polygon_vertices_in_frame(face,anchor);
        let rows=face.local_geometry.as_ref().map_or_else(||vec![Vec::new();points.len()],|g|g.incidences.clone());
        if rows.len()!=points.len(){return Err(CaeError::contract("cut local volume incidence shape differs"));}
        let constructions=face.local_geometry.as_ref().map_or_else(||vec![PointConstruction::Leaf;points.len()],|g|g.point_constructions.clone());
        if constructions.len()!=points.len(){return Err(CaeError::contract("cut local volume construction shape differs"));}
        for ((point,incidences),construction) in points.into_iter().zip(rows).zip(constructions) {
            if point
                .iter()
                .any(|v| !v.finite_first_order() || v.geometry_first_order_directions().is_none())
            {
                return Err(CaeError::contract(
                    "cut local volume has incomplete channels",
                ));
            }
            insert(
                &mut nodes,
                LocalNode {
                    construction,
                    point,
                    incidences,
                },
                anchor,
            )?;
        }
    }
    Ok(nodes.len())
}

pub(crate) fn native_face_for_polygon<S: TransientGasScalar>(face:&CutPolygon<S>)->Option<NativePlaneFace<S>> {
    let local=face.local_geometry.as_ref()?;
    let p=Plane { native_levelset:None, subdomain_face:None, native_face:None, boundary:face.boundary.clone(), normal:face.outward_normal.map(|v|-v), origin:face.plane_origin_m, offset:-face.plane_offset_m, local_origin:Some((local.anchor_m,local.plane_origin_m)) };
    local.plane_incidence.iter().chain(local.incidences.iter().flatten()).filter(|i|i.boundary==face.boundary).filter_map(|i|i.native_face.as_ref()).find(|r|r.valid(&p)).cloned()
}

pub(crate) fn subdomain_face_for_polygon<S:TransientGasScalar>(face:&CutPolygon<S>)->Option<SubdomainPlaneFace<S>> {
    let local=face.local_geometry.as_ref()?;
    let plane=Plane { native_levelset:None,native_face:None,subdomain_face:None,boundary:face.boundary.clone(),normal:face.outward_normal.map(|v|-v),origin:face.plane_origin_m,offset:-face.plane_offset_m,local_origin:Some((local.anchor_m,local.plane_origin_m))};
    local.plane_incidence.iter().chain(local.incidences.iter().flatten()).filter(|p|p.boundary==face.boundary).filter_map(|p|p.subdomain_face.as_ref()).find(|r|r.valid(&plane)).cloned()
}

pub(crate) fn native_levelset_for_polygon<S:TransientGasScalar>(face:&CutPolygon<S>)->Option<NativeLevelsetPlane<S>> {
    let local=face.local_geometry.as_ref()?;
    let plane=Plane{native_levelset:None,native_face:None,subdomain_face:None,boundary:face.boundary.clone(),normal:face.outward_normal.map(|v|-v),origin:face.plane_origin_m,offset:-face.plane_offset_m,local_origin:Some((local.anchor_m,local.plane_origin_m))};
    local.plane_incidence.iter().chain(local.incidences.iter().flatten()).filter(|p|p.boundary==face.boundary).filter_map(|p|p.native_levelset.as_ref()).find(|r|r.valid(&plane)).cloned()
}
