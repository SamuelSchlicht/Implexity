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
use implexity_core::{CaeError,CaeResult};
use crate::gas_transient_kinetic::TransientGasScalar;

#[derive(Clone,Debug)]
pub(crate) enum NativeReferencePoint {
    Grid { origin:[f64;3], index:[usize;3], spacing:f64 },
    Stored([f64;3]),
}
impl NativeReferencePoint {
    pub(crate) fn validate(&self)->CaeResult<()> {
        let valid=match self {Self::Grid{origin,index,spacing}=>origin.iter().all(|v|v.is_finite()) && spacing.is_finite() && *spacing>0. && index.iter().all(|i|(*i as u128)<=9007199254740992u128),Self::Stored(p)=>p.iter().all(|v|v.is_finite())};
        if !valid{return Err(CaeError::contract("native reference source is invalid"));}Ok(())
    }
    pub(crate) fn in_frame(&self,anchor:[f64;3])->CaeResult<[f64;3]> {
        self.validate()?;
        let mut out=[0.;3];
        for k in 0..3 {
            out[k]=match self {
                Self::Grid{origin,index,spacing}=>crate::native_exact_affine_predicate::exact_framed_grid_coordinate(origin[k],index[k],*spacing,anchor[k]),
                Self::Stored(point)=>crate::native_exact_affine_predicate::exact_framed_grid_coordinate(point[k],0,1.,anchor[k]),
            }.map_err(CaeError::contract)?;
        }
        Ok(out)
    }
    pub(crate) fn difference(&self,other:&Self)->CaeResult<[f64;3]> {
        self.validate()?;other.validate()?;
        let mut out=[0.;3];
        match (self,other) {
            (Self::Grid{origin:a,index:i,spacing:h},Self::Grid{origin:b,index:j,spacing:g}) if a.map(f64::to_bits)==b.map(f64::to_bits) && h.to_bits()==g.to_bits()=> {
                for k in 0..3 {
                    let (index,sign)=if i[k]>=j[k]{(i[k]-j[k],1.)}else{(j[k]-i[k],-1.)};
                    out[k]=sign*crate::native_exact_affine_predicate::exact_framed_grid_coordinate(0.,index,*h,0.).map_err(CaeError::contract)?;
                }
            }
            (Self::Stored(a),Self::Stored(b))=>for k in 0..3 {out[k]=crate::native_exact_affine_predicate::exact_framed_grid_coordinate(a[k],0,1.,b[k]).map_err(CaeError::contract)?;},
            _=>return Err(CaeError::contract("native coefficient reference producers differ")),
        }
        Ok(out)
    }
    pub(crate) fn same_bits(&self,other:&Self)->bool {
        match (self,other) {
            (Self::Grid{origin:a,index:i,spacing:h},Self::Grid{origin:b,index:j,spacing:g})=>a.map(f64::to_bits)==b.map(f64::to_bits) && i==j && h.to_bits()==g.to_bits(),
            (Self::Stored(a),Self::Stored(b))=>a.map(f64::to_bits)==b.map(f64::to_bits),
            _=>false,
        }
    }
}
#[derive(Clone,Debug)]
pub(crate) struct NativeNodeSource<S:Scalar> {
    pub(crate) body:usize,
    pub(crate) node:usize,
    pub(crate) force_flux:[usize;3],
    pub(crate) reference:NativeReferencePoint,
    pub(crate) displacement:[S;3],
}
impl<S:Scalar> NativeNodeSource<S> {
    pub(crate) fn map_scalars<T:Scalar>(&self,map:impl Fn(S)->T)->NativeNodeSource<T> {
        NativeNodeSource{body:self.body,node:self.node,force_flux:self.force_flux,reference:self.reference.clone(),displacement:self.displacement.map(map)}
    }
}
impl<S:TransientGasScalar> NativeNodeSource<S> {
    pub(crate) fn point_in_frame(&self,anchor:[f64;3])->CaeResult<[S;3]> {
        let reference=self.reference.in_frame(anchor)?;
        let point=std::array::from_fn(|k|self.displacement[k]+reference[k]);
        if !point.iter().copied().all(complete){return Err(CaeError::contract("native source point channels are incomplete"));}
        Ok(point)
    }
    pub(crate) fn edge_from(&self,other:&Self)->CaeResult<[S;3]> {
        if self.body!=other.body{return Err(CaeError::contract("native coefficient edge bodies differ"));}
        let reference=self.reference.difference(&other.reference)?;
        let edge=std::array::from_fn(|k|(self.displacement[k]-other.displacement[k])+reference[k]);
        if !edge.iter().copied().all(complete){return Err(CaeError::contract("native coefficient edge channels are incomplete"));}
        Ok(edge)
    }
    pub(crate) fn same(&self,other:&Self)->bool {
        self.body==other.body && self.node==other.node && self.force_flux==other.force_flux && self.reference.same_bits(&other.reference)
            && (0..3).all(|k|same(self.displacement[k],other.displacement[k]))
    }
}
#[derive(Clone,Debug)]
pub(crate) struct NativeElementSource<S:Scalar> {
    pub(crate) body:usize,
    pub(crate) element:usize,
    pub(crate) nodes:[usize;4],
    pub(crate) points:[NativeNodeSource<S>;4],
}
impl<S:Scalar> NativeElementSource<S> {
    pub(crate) fn map_scalars<T:Scalar>(&self,map:impl Fn(S)->T)->NativeElementSource<T> {
        NativeElementSource{body:self.body,element:self.element,nodes:self.nodes,points:self.points.each_ref().map(|p|p.map_scalars(&map))}
    }
}
impl<S:TransientGasScalar> NativeElementSource<S> {
    pub(crate) fn validate(&self)->CaeResult<()> {
        if (0..4).any(|i|self.nodes[i+1..].contains(&self.nodes[i]) || self.points[i].body!=self.body || self.points[i].node!=self.nodes[i]) {return Err(CaeError::contract("native coefficient source node binding differs"));}
        for point in &self.points {point.reference.validate()?;if !point.displacement.iter().copied().all(complete){return Err(CaeError::contract("native coefficient displacement is incomplete"));}}
        if self.points.iter().all(|p|matches!(p.reference,NativeReferencePoint::Stored(_))) {
            let positions=self.points.each_ref().map(|p|p.displacement.map(|v|v.value()));
            let frames=self.points.each_ref().map(|p|match p.reference {NativeReferencePoint::Stored(reference)=>reference,_=>[0.;3]});
            if !matches!(crate::native_exact_affine_predicate::exact_framed_tetra_sign(positions,frames),Ok(-1)|Ok(1)){return Err(CaeError::contract("native coefficient original tetrahedron is degenerate"));}
        }
        if let NativeReferencePoint::Grid{origin,spacing,..}=&self.points[0].reference {
            let mut indices=[[0;3];4];
            for i in 0..4 {let NativeReferencePoint::Grid{origin:other,index,spacing:h}=&self.points[i].reference else{return Err(CaeError::contract("native original reference kinds differ"));};if origin.map(f64::to_bits)!=other.map(f64::to_bits) || spacing.to_bits()!=h.to_bits(){return Err(CaeError::contract("native original reference grids differ"));}indices[i]=*index;}
            if !matches!(crate::native_exact_affine_predicate::exact_grid_displacement_tetra_sign(indices,*spacing,self.points.each_ref().map(|p|p.displacement.map(|v|v.value()))),Ok(-1)|Ok(1)){return Err(CaeError::contract("native coefficient original grid tetrahedron is degenerate"));}
        }
        let e=self.edges()?;
        if !matches!(crate::native_exact_affine_predicate::exact_framed_tetra_sign([[0.;3],e[0].map(|v|v.value()),e[1].map(|v|v.value()),e[2].map(|v|v.value())],[[0.;3];4]),Ok(-1)|Ok(1)) {return Err(CaeError::contract("native coefficient source tetrahedron is degenerate"));}
        Ok(())
    }
    pub(crate) fn edges(&self)->CaeResult<[[S;3];3]> {
        Ok([self.points[1].edge_from(&self.points[0])?,self.points[2].edge_from(&self.points[0])?,self.points[3].edge_from(&self.points[0])?])
    }
    pub(crate) fn vertices_in_frame(&self,anchor:[f64;3])->CaeResult<[[S;3];4]> {
        let mut out=[[S::zero();3];4];for i in 0..4 {out[i]=self.points[i].point_in_frame(anchor)?;}Ok(out)
    }
    pub(crate) fn face_normal(&self,indices:[usize;3])->CaeResult<[S;3]> {
        if indices.iter().any(|i|*i>=4) || indices[0]==indices[1] || indices[0]==indices[2] || indices[1]==indices[2]{return Err(CaeError::contract("native coefficient source face indices differ"));}
        let a=self.points[indices[1]].edge_from(&self.points[indices[0]])?;
        let b=self.points[indices[2]].edge_from(&self.points[indices[0]])?;
        unit(cross(a,b))
    }
    pub(crate) fn face_indices(&self,nodes:[usize;3])->Option<[usize;3]> {
        let indices=nodes.map(|n|self.nodes.iter().position(|x|*x==n));
        if indices.iter().any(Option::is_none){None}else{Some(indices.map(Option::unwrap))}
    }
    pub(crate) fn levelset(&self,values:[S;4],iso:f64)->CaeResult<([S;3],S)> {
        if !iso.is_finite() || !values.iter().copied().all(complete){return Err(CaeError::contract("native coefficient levelset controls are incomplete"));}
        let e=self.edges()?;
        let c=[cross(e[1],e[2]),cross(e[2],e[0]),cross(e[0],e[1])];
        let d=dot(e[0],c[0]);if !complete(d) || d.value()==0.{return Err(CaeError::contract("native coefficient levelset tetrahedron is degenerate"));}
        let gradient:[S;3]=std::array::from_fn(|k|((values[1]-values[0])*c[0][k]+(values[2]-values[0])*c[1][k]+(values[3]-values[0])*c[2][k])/d);
        let length=dot(gradient,gradient).sqrt();if !complete(length) || length.value()<=0.{return Err(CaeError::contract("native coefficient levelset gradient is degenerate"));}
        let normal=gradient.map(|v|v/length);let offset=(values[0]-iso)/length;
        if !normal.iter().copied().all(complete) || !complete(offset){return Err(CaeError::contract("native coefficient levelset channels are incomplete"));}
        Ok((normal,offset))
    }
    pub(crate) fn same(&self,other:&Self)->bool {self.body==other.body && self.element==other.element && self.nodes==other.nodes && (0..4).all(|i|self.points[i].same(&other.points[i]))}
}
fn complete<S:TransientGasScalar>(v:S)->bool {v.finite_first_order() && v.geometry_first_order_directions().is_some_and(|d|d.iter().all(|v|v.is_finite()))}
fn same<S:TransientGasScalar>(a:S,b:S)->bool {complete(a) && complete(b) && a.value()==b.value() && a.geometry_first_order_directions()==b.geometry_first_order_directions()}
fn dot<S:Scalar>(a:[S;3],b:[S;3])->S {(0..3).map(|k|a[k]*b[k]).fold(S::zero(),|a,b|a+b)}
fn cross<S:Scalar>(a:[S;3],b:[S;3])->[S;3] {[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]]}
fn unit<S:TransientGasScalar>(a:[S;3])->CaeResult<[S;3]> {let length=dot(a,a).sqrt();if !complete(length) || length.value()<=0.{return Err(CaeError::contract("native coefficient face is degenerate"));}let n=a.map(|v|v/length);if !n.iter().copied().all(complete){return Err(CaeError::contract("native coefficient face channels are incomplete"));}Ok(n)}

#[derive(Clone,Debug)]
pub(crate) struct NativeChartFeatureSource<S:Scalar> {
    pub(crate) feature:crate::moving_contact::surface_map::SurfaceFeature,
    pub(crate) points:Vec<NativeNodeSource<S>>,
    pub(crate) values:Vec<S>,
    pub(crate) scales:Vec<[[f64;2];3]>,
}
impl<S:Scalar> NativeChartFeatureSource<S> {
    pub(crate) fn map_scalars<T:Scalar>(&self,map:impl Fn(S)->T)->NativeChartFeatureSource<T> {
        NativeChartFeatureSource{feature:self.feature.clone(),points:self.points.iter().map(|p|p.map_scalars(&map)).collect(),values:self.values.iter().copied().map(&map).collect(),scales:self.scales.clone()}
    }
}
impl<S:TransientGasScalar> NativeChartFeatureSource<S> {
    pub(crate) fn same(&self,other:&Self)->bool {
        serde_json::to_value(&self.feature).ok().zip(serde_json::to_value(&other.feature).ok()).is_some_and(|(a,b)|a==b)
            && self.points.len()==other.points.len() && self.points.iter().zip(&other.points).all(|(a,b)|a.same(b))
            && self.values.len()==other.values.len() && self.values.iter().zip(&other.values).all(|(a,b)|same(*a,*b))
            && self.scales.len()==other.scales.len() && self.scales.iter().flatten().flatten().zip(other.scales.iter().flatten().flatten()).all(|(a,b)|a.to_bits()==b.to_bits())
    }
    pub(crate) fn weights(&self)->CaeResult<Vec<(usize,S)>> {
        if self.points.len()!=self.values.len() || self.points.len()!=self.scales.len() || self.points.is_empty() || self.values.iter().copied().any(|v|!complete(v)) || (self.scales.iter().flatten().flatten().any(|v|!v.is_finite()) || self.scales.iter().flatten().any(|pair|pair[0]==0.)) {return Err(CaeError::contract("native chart source controls are invalid"));}
        let mut density=std::collections::BTreeMap::new();
        let body=self.points[0].body;
        for (p,value) in self.points.iter().zip(&self.values) {p.reference.validate()?;if p.body!=body || !p.displacement.iter().copied().all(complete) || density.insert(p.node,*value).is_some(){return Err(CaeError::contract("native chart source node controls differ"));}}
        let weights=crate::surface_gap::weights(&self.feature,&density)?;
        if weights.len()!=self.points.len() || weights.iter().any(|(id,w)|!complete(*w) || !self.points.iter().any(|p|p.node==*id)){return Err(CaeError::contract("native chart source feature controls differ"));}
        Ok(weights)
    }
    pub(crate) fn point_in_frame(&self,anchor:[f64;3])->CaeResult<[S;3]> {
        let weights=self.weights()?;let mut point=[S::zero();3];
        for (id,weight) in weights {
            let i=self.points.iter().position(|p|p.node==id).ok_or_else(||CaeError::contract("native chart source weight node is absent"))?;
            let source=&self.points[i];let reference=source.reference.in_frame(anchor)?;
            for k in 0..3 {point[k]+=(S::from_f64(reference[k])+(source.displacement[k]/self.scales[i][k][0])*self.scales[i][k][1])*weight;}
        }
        if !point.iter().copied().all(complete){return Err(CaeError::contract("native chart source point channels are incomplete"));}Ok(point)
    }
}
#[derive(Clone,Debug)]
pub(crate) struct NativeChartPointSource<S:Scalar> {
    pub(crate) endpoints:[NativeChartFeatureSource<S>;2],
    pub(crate) fraction:f64,
}
impl<S:Scalar> NativeChartPointSource<S> {
    pub(crate) fn map_scalars<T:Scalar>(&self,map:impl Fn(S)->T)->NativeChartPointSource<T> {NativeChartPointSource{endpoints:self.endpoints.clone().map(|p|p.map_scalars(&map)),fraction:self.fraction}}
}
impl<S:TransientGasScalar> NativeChartPointSource<S> {
    pub(crate) fn point_in_frame(&self,anchor:[f64;3])->CaeResult<[S;3]> {
        if !self.fraction.is_finite() || !(0. ..=1.).contains(&self.fraction){return Err(CaeError::contract("native chart source layer fraction is invalid"));}
        let a=self.endpoints[0].point_in_frame(anchor)?;let b=self.endpoints[1].point_in_frame(anchor)?;
        let point=std::array::from_fn(|k|a[k]+(b[k]-a[k])*self.fraction);
        if !point.iter().copied().all(complete){return Err(CaeError::contract("native chart layer source channels are incomplete"));}Ok(point)
    }
    pub(crate) fn same(&self,other:&Self)->bool {self.fraction.to_bits()==other.fraction.to_bits() && self.endpoints.iter().zip(&other.endpoints).all(|(a,b)|a.same(b))}
    pub(crate) fn boundary_feature(&self)->Option<&NativeChartFeatureSource<S>> {if self.fraction==0.{Some(&self.endpoints[0])}else if self.fraction==1.{Some(&self.endpoints[1])}else{None}}
}
