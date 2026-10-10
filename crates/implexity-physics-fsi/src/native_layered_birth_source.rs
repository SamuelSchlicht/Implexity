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

use crate::{gas_geometry_time::{GeometryTime,GeometryTimeBase},gas_layered_tetra_mesh::{self,LayeredGapTriangle},native_gas_geometry_adapter::NativeGasGeometryAdapter,native_normal_birth::{NativeNormalBirthContext,NativeNormalBirthStage}};
use implexity_ad::Scalar;
use implexity_core::{CaeError,CaeResult};
use std::{collections::BTreeMap,sync::Arc};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
fn same<S:GeometryTimeBase>(a:GeometryTime<S>,b:GeometryTime<S>)->bool{
 a.value().to_bits()==b.value().to_bits() && match(a.geometry_first_order_directions(),b.geometry_first_order_directions()){
 (Some(x),Some(y))=>x.len()==y.len()&&x.iter().zip(y).all(|(a,b)|a.to_bits()==b.to_bits()),_=>false}
}
use crate::gas_transient_kinetic::TransientGasScalar;
pub(crate) struct NativeLayeredBirthSource<'a,S:GeometryTimeBase>{
 context:Arc<NativeNormalBirthContext<'a,S>>,adapter:&'a NativeGasGeometryAdapter,chart:usize,corner:usize,layer:usize,layers:usize,node:usize,paired_vertex:usize,original_source:(usize,usize,usize),point:[GeometryTime<S>;3],tetrahedra:Vec<(usize,[usize;4])>,
}
impl<'a,S:GeometryTimeBase> NativeLayeredBirthSource<'a,S>{
 pub(crate) fn chart(&self)->usize{self.chart}
 pub(crate) fn corner(&self)->usize{self.corner}
 pub(crate) fn layer(&self)->usize{self.layer}
 pub(crate) fn layers(&self)->usize{self.layers}
 pub(crate) fn node(&self)->usize{self.node}
 pub(crate) fn paired_vertex(&self)->usize{self.paired_vertex}
 pub(crate) fn original_source(&self)->(usize,usize,usize){self.original_source}
 pub(crate) fn point(&self)->[GeometryTime<S>;3]{self.point}
 pub(crate) fn tetrahedra(&self)->&[(usize,[usize;4])]{&self.tetrahedra}
 pub(crate) fn validate_owner(&self,adapter:&NativeGasGeometryAdapter,stage:&NativeNormalBirthStage<'a,S>)->CaeResult<()>{
 if !std::ptr::eq(self.adapter,adapter) || !Arc::ptr_eq(&self.context,stage.context()?) || self.chart!=stage.chart(){return Err(fail("layered birth source belongs to another actual owner"));}Ok(())
 }
}
pub(crate) struct NativeLayeredBirthSources<'a,S:GeometryTimeBase>{sources:Vec<NativeLayeredBirthSource<'a,S>>}
impl<'a,S:GeometryTimeBase> NativeLayeredBirthSources<'a,S>{
 pub(crate) fn from_stage(adapter:&'a NativeGasGeometryAdapter,stage:&NativeNormalBirthStage<'a,S>)->CaeResult<Self>{
 let context=stage.context()?.clone();
 if context.binding_identity()!=adapter.kinematics.binding_identity(){return Err(fail("layered birth source feature binding differs"));}
 let authenticated=NativeNormalBirthStage::from_shared_context(&adapter.kinematics,adapter.owner.capacity_owner(),context.clone(),stage.chart(),stage.bindings().clone())?;
 if (0..6).any(|i|(0..3).any(|a|!same(stage.vertices()[i][a],authenticated.vertices()[i][a]))){return Err(fail("layered birth stage realization differs"));}
 if adapter.layers==0 || adapter.paired_vertex_ids.len()!=adapter.kinematics.cell_count(){return Err(fail("layered birth adapter chart shape differs"));}
 let step=S::from_f64(context.nominal_tick_step_s())*context.time_scale();
 let trace=context.child_start().iter().zip(context.child_end()).map(|(&a,&b)|GeometryTime{value:a,rate:(b-a)/step}).collect::<Vec<_>>();
 let design=context.design().iter().map(|&v|GeometryTime{value:v,rate:S::zero()}).collect::<Vec<_>>();
 let mapped=adapter.kinematics.mapped_geometry_from_trace(&trace,&design,|v,_|Ok(v))?;
 let end=adapter.kinematics.mapped_geometry_from_trace(context.child_end(),context.design(),|v,_|Ok(v))?;
 if mapped.len()!=adapter.paired_vertex_ids.len()||end.len()!=mapped.len(){return Err(fail("layered birth mapped shape differs"));}
 let triangles=end.iter().zip(&adapter.paired_vertex_ids).map(|(&p,&ids)|LayeredGapTriangle{paired_vertex_ids:ids,start_m:p,end_m:p}).collect::<Vec<_>>();
 let mesh=gas_layered_tetra_mesh::build_closed_endpoint(&triangles,adapter.layers,S::one(),adapter.length_tolerance_m,context.sample().closed_end_charts(),context.sample().closed_end_charts(),1.)?;
 let mut ids=BTreeMap::<(usize,usize),usize>::new();let mut points=Vec::<[GeometryTime<S>;3]>::new();let mut provenance=Vec::new();let mut requested=Vec::new();
 for (chart,p) in mapped.iter().enumerate(){for corner in 0..3{
 let fixed=(0..3).all(|a|{let d=p[corner+3][a]-p[corner][a];d.value()==0.&&d.maximum_direction()==0.});
 for layer in 0..=adapter.layers{
 let point=std::array::from_fn(|a|p[corner][a]+(p[corner+3][a]-p[corner][a])*(layer as f64/adapter.layers as f64));
 let key=(adapter.paired_vertex_ids[chart][corner],if fixed{0}else{layer});
 let node=if let Some(&node)=ids.get(&key){if (0..3).any(|a|!same(points[node][a],point[a])){return Err(fail("shared birth source has unequal full motion"));}node}else{let node=points.len();ids.insert(key,node);points.push(point);provenance.push((chart,corner,layer,key));node};
 if chart==stage.chart()&&(0..3).any(|a|!same(point[a],stage.vertices()[corner][a]+(stage.vertices()[corner+3][a]-stage.vertices()[corner][a])*(layer as f64/adapter.layers as f64))){return Err(fail("birth source interpolation differs from authenticated chart"));}
 if chart==stage.chart(){requested.push((node,chart,corner,layer));}
 }}}
 if mesh.nodes.len()!=points.len(){return Err(fail("finite birth source topology differs from right-time lookup"));}
 for (i,n) in mesh.nodes.iter().enumerate(){let key=provenance[i].3;let (c,k,l,_)=provenance[i];let point:[S;3]=std::array::from_fn(|a|end[c][k][a]+(end[c][k+3][a]-end[c][k][a])*(l as f64/adapter.layers as f64));if (0..3).any(|a|n.end_m[a].value().to_bits()!=point[a].value().to_bits()||match(n.end_m[a].geometry_first_order_directions(),point[a].geometry_first_order_directions()){(Some(x),Some(y))=>x.len()!=y.len()||x.iter().zip(y).any(|(a,b)|a.to_bits()!=b.to_bits()),_=>true})||n.paired_vertex!=key.0||n.layer!=key.1{return Err(fail("finite birth source semantic node differs"));}}
 let mut sources=Vec::new();
 for &(node,chart,corner,layer) in &requested{let (original_chart,original_corner,original_layer,key)=provenance[node];
 let tetrahedra=mesh.tetrahedra.iter().filter(|t|t.chart==chart&&t.volume_m3.value()>0.&&t.nodes.contains(&node)).map(|t|((t.chart*adapter.layers+t.layer)*3+t.local_tetra,t.nodes)).collect::<Vec<_>>();
 if tetrahedra.is_empty(){continue;}
 sources.push(NativeLayeredBirthSource{context:context.clone(),adapter,chart,corner,layer,layers:adapter.layers,node,paired_vertex:key.0,original_source:(original_chart,original_corner,original_layer),point:points[node],tetrahedra});
 }
 if sources.is_empty(){return Err(fail("birth chart has no authentic positive endpoint tetrahedral source"));}
 Ok(Self{sources})
 }
 pub(crate) fn sources(&self)->&[NativeLayeredBirthSource<'a,S>]{&self.sources}
 pub(crate) fn node_in_region(&self,node:usize,region:usize)->CaeResult<&NativeLayeredBirthSource<'a,S>>{self.sources.iter().find(|s|s.node==node&&s.tetrahedra.iter().any(|t|t.0==region)).ok_or_else(||fail("birth node is absent from actual source tetrahedron"))}
}
