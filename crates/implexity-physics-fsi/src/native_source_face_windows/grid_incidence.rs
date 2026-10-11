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
use super::{FixedGridPlane,NativeSourceFaceWindows,SourceWindowKind,fail};
use crate::gas_geometry_time::{GeometryTime,GeometryTimeBase};
use crate::gas_gap_mass::GasMassScalar;
use crate::gas_transient_kinetic::TransientGasScalar;
use implexity_ad::Scalar;
use implexity_core::CaeResult;

pub(crate) struct NativeGridCornerIncidence<'a,'g,B:GeometryTimeBase>{
 source:&'a NativeSourceFaceWindows<'g,B>,plane:&'a FixedGridPlane,window:usize,corner:usize,
}
impl<'g,B:GeometryTimeBase> NativeSourceFaceWindows<'g,B>{
 pub(crate) fn native_grid_corner_incidence<'a>(&'a self,window:usize,plane:&'a FixedGridPlane,corner:usize)->CaeResult<Option<NativeGridCornerIncidence<'a,'g,B>>>{
  if self.native_grid_corner_distance(window,plane,corner)?.is_none(){return Ok(None);}
  Ok(Some(NativeGridCornerIncidence{source:self,plane,window,corner}))
 }
 fn native_grid_corner_distance(&self,window:usize,plane:&FixedGridPlane,corner:usize)->CaeResult<Option<GeometryTime<B>>>{
  let w=self.windows.get(window).ok_or_else(||fail("native grid incidence window is absent"))?;
  if corner>=3{return Err(fail("native grid incidence corner is absent"));}
  if !matches!(w.kind(),SourceWindowKind::NativePhysical){return Ok(None);}
  if !self.grid_planes.iter().any(|p|std::ptr::eq(p,plane)){return Err(fail("native grid incidence plane owner differs"));}
  let owner=self.adapter.owner.capacity_owner();
  let Some(grids)=&owner.reference_grids else{return Ok(None);};
  let feature=&w.facet().vertex_features[corner];
  let (body,nodes)=match feature.as_slice(){
   [0,body,node] if *body<2=>(*body,vec![*node]),
   [1,body,a,b] if *body<2&&a<b=>(*body,vec![*a,*b]),
   _=>return Ok(None),
  };
  let authentic=match w.facet().owner.as_slice(){
   [1,facet_body,element] if *facet_body==body=>owner.elements[body].get(*element).is_some_and(|e|nodes.iter().all(|n|e.contains(n))),
   [0,facet_body,a,b,c] if *facet_body==body=>nodes.iter().all(|n|[*a,*b,*c].contains(n)),
   _=>false,
  };
  if !authentic{return Err(fail("native grid incidence feature owner differs"));}
  let native=&grids[body];let fluid=&owner.grid;let axis=plane.axis();
  if native.element_size_m.to_bits()!=fluid.spacing_m.to_bits()||native.node_count()!=owner.nodes[body].len(){return Ok(None);}
  if plane.coordinate_m().to_bits()!=(fluid.origin_m[axis]+plane.index() as f64*fluid.spacing_m).to_bits(){return Err(fail("native grid incidence plane producer differs"));}
  let first=*nodes.first().ok_or_else(||fail("native grid incidence feature nodes are absent"))?;
  if first>=native.node_count(){return Err(fail("native grid incidence node is absent"));}
  let native_index=native.node_ijk(first)[axis];
  if plane.index() as u128>(1u128<<53)||native_index as u128>(1u128<<53){return Ok(None);}
  let offset=plane.index() as f64-native_index as f64;
  let Ok(sign)=crate::native_exact_affine_predicate::exact_sign([0.,fluid.spacing_m,fluid.origin_m[axis]],[offset,1.],native.origin_m[axis]) else{return Ok(None);};
  if sign!=0{return Ok(None);}
  let reference=native.node_position(first)[axis];
  let mut displacements=Vec::with_capacity(nodes.len());
  for node in nodes{
   if node>=native.node_count(){return Err(fail("native grid incidence node is absent"));}
   let binding=&owner.nodes[body][node];
   if native.node_ijk(node)[axis]!=native_index{return Ok(None);}
   if native.node_position(node).map(f64::to_bits)!=binding.reference_m.map(f64::to_bits){return Err(fail("native grid incidence reference producer differs"));}
   let displacement=*self.live_trace.get(binding.force_flux[axis]).ok_or_else(||fail("native grid incidence trace is absent"))?;
   displacements.push(displacement);
  }
  if displacements.iter().any(|d|d.value()!=0.){return Ok(None);}
  for displacement in displacements{
   if !displacement.finite_first_order()||!displacement.geometry_first_order_directions().is_some_and(|d|d.iter().all(|v|v.is_finite()&&*v==0.)){
    return Err(fail("native grid incidence crosses the fixed plane stratum"));
   }
  }
  let point=w.facet().vertices[corner][axis];
  if point.value().to_bits()!=reference.to_bits(){return Err(fail("native grid incidence corner producer differs"));}
  if !point.finite_first_order()||!point.geometry_first_order_directions().is_some_and(|d|d.iter().all(|v|v.is_finite()&&*v==0.)){
   return Err(fail("native grid incidence corner crosses the fixed plane stratum"));
  }
  let distance=point-plane.coordinate_m();
  Ok(Some(distance-distance.value()))
 }
}
impl<B:GeometryTimeBase> NativeGridCornerIncidence<'_,'_,B>{
 pub(crate) fn distance(&self,source:&NativeSourceFaceWindows<'_,B>,window:usize,plane:&FixedGridPlane,corner:usize)->CaeResult<GeometryTime<B>>{
  if !std::ptr::eq(self.source,source)||!std::ptr::eq(self.plane,plane)||self.window!=window||self.corner!=corner{return Err(fail("native grid incidence invocation differs"));}
  self.source.native_grid_corner_distance(window,plane,corner)?.ok_or_else(||fail("native grid incidence proof is absent"))
 }
}

pub(crate) trait NativeGridAffineSource{
 fn grid_affine_corners(&self,window:usize,axis:usize,coordinate:f64)->CaeResult<Option<[f64;3]>>;
 fn source_owns_physical_plane(&self,_window:usize,_plane:&crate::native_cut_cell_partition::CutPolygon<f64>)->CaeResult<bool>{Ok(false)}
 fn source_local_corners(&self,_window:usize,_anchor:[f64;3])->CaeResult<Option<[[f64;3];3]>>{Ok(None)}
}
impl<B:GeometryTimeBase> NativeGridAffineSource for NativeSourceFaceWindows<'_,B>{
 fn source_owns_physical_plane(&self,window:usize,plane:&crate::native_cut_cell_partition::CutPolygon<f64>)->CaeResult<bool>{self.owns_physical_plane(window,plane)}
 fn source_local_corners(&self,window:usize,anchor:[f64;3])->CaeResult<Option<[[f64;3];3]>>{Ok(self.source_corners_in_frame(window,anchor)?.map(|c|c.map(|p|p.map(|v|v.value()))))}
 fn grid_affine_corners(&self,window:usize,axis:usize,coordinate:f64)->CaeResult<Option<[f64;3]>>{
  let w=self.windows.get(window).ok_or_else(||fail("native grid affine window is absent"))?;
  if axis>=3||!coordinate.is_finite(){return Err(fail("native grid affine plane is invalid"));}
  let Some(plane)=self.grid_planes.iter().find(|p|p.axis()==axis&&p.coordinate_m().to_bits()==coordinate.to_bits()) else{return Ok(None);};
  let mut corners=w.facet().vertices.map(|p|p[axis].value());
  for corner in 0..3{if let Some(proof)=self.native_grid_corner_incidence(window,plane,corner)?{
   let distance=proof.distance(self,window,plane,corner)?;
   if distance.value()!=0.||!distance.geometry_first_order_directions().is_some_and(|d|d.iter().all(|v|v.is_finite()&&*v==0.)){return Err(fail("native grid affine incidence proof differs"));}
   corners[corner]=coordinate;
  }}
  Ok(Some(corners))
 }
}

impl<B:GeometryTimeBase> NativeSourceFaceWindows<'_,B>{
 pub(crate) fn source_corners_in_frame(&self,window:usize,anchor:[f64;3])->CaeResult<Option<[[GeometryTime<B>;3];3]>>{
  let w=self.windows.get(window).ok_or_else(||fail("native local source window is absent"))?;
  if anchor.iter().any(|v|!v.is_finite()){return Err(fail("native local source anchor is invalid"));}
  let owner=self.adapter.owner.capacity_owner();
  if matches!(w.kind(),SourceWindowKind::Layered{..}){
   let mut corners=[[GeometryTime::<B>::zero();3];3];
   for (i,feature) in w.facet().vertex_features.iter().enumerate(){
    let [2,paired,layer]=feature.as_slice()else{return Err(fail("local layered source feature is invalid"))};
    if self.adapter.layers==0||*layer>self.adapter.layers{return Err(fail("local layered source layer is invalid"));}
    let (chart,corner)=self.adapter.paired_vertex_ids.iter().enumerate().find_map(|(chart,ids)|ids.iter().position(|id|id==paired).map(|corner|(chart,corner))).ok_or_else(||fail("local layered source chart is absent"))?;
    let p=self.adapter.kinematics.mapped_chart_from_reference(chart,&self.live_trace,&self.live_design,|body,node|owner.node_reference_in_frame(body,node,anchor),|p,_|Ok(p))?;
    let t=*layer as f64/self.adapter.layers as f64;
    let point=std::array::from_fn::<_,3,_>(|k|p[corner][k]+(p[corner+3][k]-p[corner][k])*t);
    if point.iter().any(|v|!v.finite_first_order()||v.geometry_first_order_directions().is_none()){return Err(fail("local layered source channels are incomplete"));}
    corners[i]=point;
   }
   return Ok(Some(corners));
  }
  let Some(grids)=&owner.reference_grids else{return Ok(None)};
  let body=w.facet().owner.get(1).copied().filter(|b|*b<2).ok_or_else(||fail("native local source body is absent"))?;
  let grid=&grids[body];
  let node=|id:usize|->CaeResult<[GeometryTime<B>;3]>{
   let binding=owner.nodes[body].get(id).ok_or_else(||fail("native local source node is absent"))?;
   if grid.node_position(id).map(f64::to_bits)!=binding.reference_m.map(f64::to_bits){return Err(fail("native local source grid binding differs"));}
   let ijk=grid.node_ijk(id);
   Ok(std::array::from_fn(|k|self.live_trace[binding.force_flux[k]]+((grid.origin_m[k]-anchor[k])+ijk[k]as f64*grid.element_size_m)))
  };
  let phase=|id:usize|->CaeResult<GeometryTime<B>>{
   if id>=owner.nodes[body].len(){return Err(fail("native local source phase node is absent"));}
   let row=id+if body==0{0}else{owner.nodes[0].len()};let(columns,weights)=owner.phase.row(row);
   Ok(columns.iter().zip(weights).map(|(&j,&v)|self.live_design[j]*v).fold(GeometryTime::<B>::zero(),|a,b|a+b))
  };
  let mut corners=[[GeometryTime::<B>::zero();3];3];
  for (corner,feature) in w.facet().vertex_features.iter().enumerate(){
   let ids=match feature.as_slice(){[0,b,id]if *b==body=>vec![*id],[1,b,a,c]if *b==body&&a<c=>vec![*a,*c],_=>return Err(fail("native local source feature is invalid"))};
   let authentic=match w.facet().owner.as_slice(){[1,b,element]if *b==body=>owner.elements[body].get(*element).is_some_and(|e|ids.iter().all(|id|e.contains(id))),[0,b,a,c,d]if *b==body=>ids.iter().all(|id|[*a,*c,*d].contains(id)),_=>false};
   if !authentic{return Err(fail("native local source feature owner differs"));}
   let point=if ids.len()==1{node(ids[0])?}else{let a=node(ids[0])?;let b=node(ids[1])?;let pa=phase(ids[0])?;let pb=phase(ids[1])?;let t=(GeometryTime::<B>::from_f64(owner.iso)-pa)/(pb-pa);std::array::from_fn(|k|a[k]+(b[k]-a[k])*t)};
   if point.iter().any(|v|!v.finite_first_order()||v.geometry_first_order_directions().is_none()){return Err(fail("native local source channels are incomplete"));}
   corners[corner]=point;
  }
  Ok(Some(corners))
 }
}
