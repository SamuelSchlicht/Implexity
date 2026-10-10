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

use crate::gas_geometry_time::GeometryTimeBase;
use crate::gas_gap_mass::GasMassScalar;
use crate::gas_transient_kinetic::TransientGasScalar;
use crate::native_cut_cell_refinement::{self,CutTetraSubdomain,RefinedCutCellPartition};
use crate::native_source_face_windows::{NativeSourceFaceWindows,SourceWindowKind,FixedGridPlane};
use crate::native_union_slot_index::UnionSlotIndex;
use crate::native_source_facet_index::SourceFacetIndex;
use implexity_ad::Scalar;
use implexity_core::{CaeError,CaeResult};
fn fail(message:&str)->CaeError{CaeError::contract(message)}
pub(crate) struct NativeSourceStageLabels<'a,'g,S:GeometryTimeBase>{source:&'a NativeSourceFaceWindows<'g,S>,refined:RefinedCutCellPartition<f64>,component_slots:Vec<usize>,authority:std::sync::Arc<()>}
pub(crate) struct PreparedSourceStageLabels<'p,'a,'g,S:GeometryTimeBase>{owner:&'p NativeSourceStageLabels<'a,'g,S>,index:UnionSlotIndex<'p>,facets:SourceFacetIndex<'a,'g,S>,axis_planes:[&'a[FixedGridPlane];3],native_parents:[std::collections::BTreeMap<[usize;3],Vec<usize>>;2]}
#[derive(Clone,Debug)]
pub(crate) struct NativeWallOwned{invocation:crate::native_union_arrangement::OutputSweepInvocation,authority:std::sync::Arc<()>,window:usize,fluid:usize,cell:usize,component:usize,part:usize,plane:usize,geometry_bits:Vec<u64>}
impl PartialEq for NativeWallOwned{fn eq(&self,other:&Self)->bool{self.invocation==other.invocation&&std::sync::Arc::ptr_eq(&self.authority,&other.authority)&&self.window==other.window&&self.fluid==other.fluid&&self.cell==other.cell&&self.component==other.component&&self.part==other.part&&self.plane==other.plane&&self.geometry_bits==other.geometry_bits}}
impl Eq for NativeWallOwned{}
#[derive(Clone,Debug,PartialEq,Eq)]
pub(crate) enum SourceFaceLabel{NativeWall{fluid:usize},NativeWallOwned(NativeWallOwned),LayeredInterface{minus:usize,plus:usize}}
impl<'a,'g,S:GeometryTimeBase> NativeSourceStageLabels<'a,'g,S>{
 pub(crate) fn new(source:&'a NativeSourceFaceWindows<'g,S>)->CaeResult<Self>{
  let adapter=source.adapter();let base=adapter.owner.partition(source.stage_trace(),source.stage_design())?;
  let tets=source.stage_tets().iter().map(|t|CutTetraSubdomain{region:t.region,vertex_ids:t.vertex_ids,vertices_m:t.vertices_m.map(|p|p.map(|v|v.value()))}).collect::<Vec<_>>();
  let refined=native_cut_cell_refinement::refine(&base,&tets)?;let aggregated=adapter.layout.aggregate(&refined)?;
  Ok(Self{source,refined,component_slots:aggregated.component_slots,authority:std::sync::Arc::new(())})
 }
 pub(crate) fn prepare(&self)->CaeResult<PreparedSourceStageLabels<'_,'a,'g,S>>{let grid=&self.source.adapter().owner.capacity_owner().grid;let mut start=0;let axis_planes=std::array::from_fn(|axis|{let end=start+grid.shape[axis]+1;let slice=&self.source.grid_planes()[start..end];start=end;slice});if start!=self.source.grid_planes().len()||axis_planes.iter().enumerate().any(|(axis,p)|p.iter().any(|p|p.axis()!=axis)){return Err(fail("source stage grid plane coverage differs"));}Ok(PreparedSourceStageLabels{owner:self,index:UnionSlotIndex::new(&self.refined,&self.component_slots)?,facets:SourceFacetIndex::new(self.source)?,axis_planes,native_parents:source_native_parents(self.source)})}
 pub(crate) fn component_slots(&self)->&[usize]{&self.component_slots}
 pub(crate) fn refined(&self)->&RefinedCutCellPartition<f64>{&self.refined}
}
impl<'p,'a,'g,S:GeometryTimeBase> PreparedSourceStageLabels<'p,'a,'g,S>{
 pub(crate) fn source(&self)->&'a NativeSourceFaceWindows<'g,S>{self.owner.source}
 fn source_native_facet(&self,window:usize)->CaeResult<SourceNativeFacetIdentity<'_>>{
  let source=self.source();let w=source.windows().get(window).ok_or_else(||fail("native source window is absent"))?;
  if !matches!(w.kind(),SourceWindowKind::NativePhysical){return Err(fail("native facet identity requires actual physical source"));}
  let owner=source.adapter().owner.capacity_owner();
  let(body,element,face)=match w.facet().owner.as_slice(){
   [1,body,element]if *body<2&&*element<owner.elements[*body].len()=>(*body,*element,None),
   [0,body,a,b,c]if *body<2=>{let mut nodes=[*a,*b,*c];nodes.sort();let parents=self.native_parents[*body].get(&nodes).ok_or_else(||fail("native source exterior parent is absent"))?;if parents.len()!=1{return Err(fail("native source exterior parent is not unique"));}(*body,parents[0],Some(nodes))},
   _=>return Err(fail("native physical source identity is malformed")),
  };Ok(SourceNativeFacetIdentity{refined:&self.owner.refined,body,element,face,grid_alias:NativeGridSupportAlias::issue(source,window)?})
 }
 pub(crate) fn native_wall_parent(&self,window:usize)->CaeResult<(usize,usize,[usize;4])>{let f=self.source_native_facet(window)?;Ok((f.body,f.element,self.source().adapter().owner.capacity_owner().elements[f.body][f.element]))}
 pub(crate) fn validate_native_wall_owned(&self,token:&NativeWallOwned,window:usize,uv:[crate::gas_geometry_time::GeometryTime<S>;2],point:[crate::gas_geometry_time::GeometryTime<S>;3],normal:[crate::gas_geometry_time::GeometryTime<S>;3],output_receipt:&crate::native_union_arrangement::OutputSweepReceipt)->CaeResult<(usize,usize,usize,usize)>{
  if !std::sync::Arc::ptr_eq(&token.authority,&self.owner.authority)||token.window!=window||wall_geometry_bits(uv,point,normal)?!=token.geometry_bits||!matches!(self.source().windows().get(window).map(|w|w.kind()),Some(SourceWindowKind::NativePhysical)){return Err(fail("source wall quadrature identity differs"));}
  let live_normal=normal;output_receipt.validate_invocation(&token.invocation).map_err(fail)?;let receipt=output_receipt.validate(uv).map_err(fail)?;let uv=uv.map(|v|v.value());let point=point.map(|v|v.value());let normal=normal.map(|v|v.value());let query=self.source_sweep_query(window,uv,point,receipt)?;let facet=self.source_native_facet(window)?;
  if self.cell_sweep(window,uv,point,normal,receipt)?!=Some(token.cell)||self.index.support_for_slot_at_face_filtered(token.cell,point,normal,token.fluid,&query,&facet)?!=(token.component,token.part,token.plane){return Err(fail("source wall quadrature support differs"));}
  facet.validate_emitted_support(&self.owner.refined,token.cell,token.component,token.part,token.plane,live_normal)?;
  Ok((token.fluid,token.component,token.part,token.plane))
 }
 pub(crate) fn validate_native_wall_support(&self,token:&NativeWallOwned,window:usize,sample:&crate::native_physical_plane_preparation::PhysicalPlaneSample<crate::gas_geometry_time::GeometryTime<S>,SourceFaceLabel>,actual:&crate::native_cut_cell_partition::CutPolygon<crate::gas_geometry_time::GeometryTime<S>>)->CaeResult<()>{
  self.validate_native_wall_owned(token,window,sample.source_uv,sample.position_m,sample.normal,&sample.source_receipt)?;
  let facet=self.source_native_facet(window)?;
  match &actual.boundary{
   crate::native_cut_cell_partition::CutBoundary::Native{body,element,face,physical:true}=>{let mut face=*face;if let Some(ref mut f)=face{f.sort();}if *body!=facet.body||*element!=facet.element||face!=facet.face{return Err(fail("source support actual Native plane differs"));}Ok(())},
   crate::native_cut_cell_partition::CutBoundary::Cell{..}=>{
    let alias=facet.grid_alias.as_ref().ok_or_else(||fail("source wall has no authenticated native grid alias"))?;
    let bounds=alias.grid.cell_bounds(token.cell)?;
    if !alias.matches(actual,bounds)||!alias.normal_matches(sample.normal){return Err(fail("source wall grid support full scalar coincidence differs"));}Ok(())
   },
   _=>Err(fail("source support plane is not physical Native or authenticated grid support")),
  }
 }
 pub(crate) fn native_wall_component(&self,point:[f64;3],solid_outnormal:[f64;3],expected_slot:usize)->CaeResult<usize>{let cell=self.cell(point,solid_outnormal)?.ok_or_else(||fail("native wall fluid cell is absent"))?;self.index.component_for_slot_at_face(cell,point,solid_outnormal,expected_slot)}
 pub(crate) fn prepare_window(&self,window:usize,refinement:usize,frame:[f64;2])->CaeResult<Vec<crate::native_physical_plane_preparation::PhysicalPlaneSample<crate::gas_geometry_time::GeometryTime<S>,SourceFaceLabel>>>{
  let source=self.owner.source;let w=source.windows().get(window).ok_or_else(||fail("source window index is out of range"))?;
  let candidates=self.facets.candidate_indices(window)?;
  let mut samples=crate::native_physical_plane_preparation::prepare_owned_source_plane_face_sweep(source,window,&candidates,refinement,frame,|point,normal,uv,receipt|self.classify_window_sweep(window,uv,point,normal,receipt))?;
  let native_facet=if matches!(w.kind(),SourceWindowKind::NativePhysical){Some(self.source_native_facet(window)?)}else{None};
  for sample in &mut samples{
   let receipt=sample.source_receipt.validate(sample.source_uv).map_err(fail)?;
   if self.classify_window_sweep(window,sample.source_uv.map(|v|v.value()),sample.position_m.map(|v|v.value()),sample.normal.map(|v|v.value()),receipt)?!=Some(sample.label.clone()){return Err(fail("source quadrature label differs from its region representative"));}
   if let SourceFaceLabel::NativeWall{fluid}=sample.label{
    let point=sample.position_m.map(|v|v.value());let normal=sample.normal.map(|v|v.value());let uv=sample.source_uv.map(|v|v.value());
    
    let cell=self.cell_sweep(window,uv,point,normal,receipt)?.ok_or_else(||fail("source wall quadrature cell is absent"))?;
    let query=self.source_sweep_query(window,uv,point,receipt)?;
    let(component,part,plane)=self.index.support_for_slot_at_face_filtered(cell,point,normal,fluid,&query,native_facet.as_ref().ok_or_else(||fail("native source facet binding is absent"))?)?;
    native_facet.as_ref().ok_or_else(||fail("native source facet binding is absent"))?.validate_emitted_support(&self.owner.refined,cell,component,part,plane,sample.normal)?;
    let geometry_bits=wall_geometry_bits(sample.source_uv,sample.position_m,sample.normal)?;
    sample.label=SourceFaceLabel::NativeWallOwned(NativeWallOwned{invocation:sample.source_receipt.invocation(),authority:self.owner.authority.clone(),window,fluid,cell,component,part,plane,geometry_bits});
   }
  }Ok(samples)
 }
 fn cell_sweep(&self,window:usize,uv:[f64;2],point:[f64;3],inward:[f64;3],receipt:&crate::native_union_arrangement::SweepPointReceipt)->CaeResult<Option<usize>>{
  receipt.validate_uv(uv).map_err(fail)?;let source=self.owner.source;let grid=&source.adapter().owner.capacity_owner().grid;
  let facet=source.windows().get(window).ok_or_else(||fail("affine grid window is absent"))?.facet();
  let corners=facet.vertices.map(|p|p.map(|v|v.value()));
  if facet.vertices.iter().flatten().any(|v|!v.finite_first_order()||v.geometry_first_order_directions().is_none())||uv.iter().chain(point.iter()).chain(inward.iter()).any(|v|!v.is_finite())||inward.iter().all(|v|*v==0.){return Err(fail("affine grid geometry is invalid"));}
  for corner in 0..3{if receipt.barycentric_sign(corner).map_err(fail)?!=1{return Err(fail("affine grid representative is not certified inside its source triangle"));}}
  let origin=corners[0];let u=std::array::from_fn::<_,3,_>(|k|corners[1][k]-origin[k]);let v=std::array::from_fn::<_,3,_>(|k|corners[2][k]-origin[k]);
  if(0..3).any(|k|(origin[k]+u[k]*uv[0]+v[k]*uv[1]).to_bits()!=point[k].to_bits()){return Err(fail("affine grid point is not bound to its actual source window"));}
  let mut ijk=[0usize;3];for axis in 0..3{
   let planes=self.axis_planes[axis];if planes.len()!=grid.shape[axis]+1{return Err(fail("affine grid plane coverage differs"));}
   let sign=|coordinate:f64|receipt.affine_sign(corners.map(|p|p[axis]),coordinate).map_err(fail);
   let(mut lo,mut hi)=(0,planes.len());let mut equal=None;
   while lo<hi{let mid=lo+(hi-lo)/2;match sign(planes[mid].coordinate_m())?{1=>lo=mid+1,-1=>hi=mid,0=>{equal=Some(mid);break;},_=>return Err(fail("affine grid sign is invalid"))}}
   let index=if let Some(i)=equal{if inward[axis]<0.{i.checked_sub(1)}else if inward[axis]>0.{Some(i)}else{return Err(fail("source face lies on an unresolved exact affine grid tangent incidence"));}}else{lo.checked_sub(1)};
   let Some(index)=index else{return Ok(None)};if index>=grid.shape[axis]{return Ok(None)}ijk[axis]=index;
  }
  let cell=(ijk[0]*grid.shape[1]+ijk[1])*grid.shape[2]+ijk[2];Ok(self.owner.refined.partition.native_capacity_m3.contains_key(&cell).then_some(cell))
 }
 fn cell_affine(&self,window:usize,uv:[f64;2],point:[f64;3],inward:[f64;3])->CaeResult<Option<usize>>{
  let source=self.owner.source;let grid=&source.adapter().owner.capacity_owner().grid;
  let facet=source.windows().get(window).ok_or_else(||fail("affine grid window is absent"))?.facet();
  let corners=facet.vertices.map(|p|p.map(|v|v.value()));
  if facet.vertices.iter().flatten().any(|v|!v.finite_first_order()||v.geometry_first_order_directions().is_none())||uv.iter().chain(point.iter()).chain(inward.iter()).any(|v|!v.is_finite())||inward.iter().all(|v|*v==0.){return Err(fail("affine grid geometry is invalid"));}
  for corner in 0..3{if crate::native_affine_grid_sign::barycentric_sign(uv,corner)?!=1{return Err(fail("affine grid representative is not certified inside its source triangle"));}}
  let origin=corners[0];let u=std::array::from_fn::<_,3,_>(|k|corners[1][k]-origin[k]);let v=std::array::from_fn::<_,3,_>(|k|corners[2][k]-origin[k]);
  if(0..3).any(|k|(origin[k]+u[k]*uv[0]+v[k]*uv[1]).to_bits()!=point[k].to_bits()){return Err(fail("affine grid point is not bound to its actual source window"));}
  let mut ijk=[0usize;3];for axis in 0..3{
   let planes=self.axis_planes[axis];if planes.len()!=grid.shape[axis]+1{return Err(fail("affine grid plane coverage differs"));}
   let sign=|coordinate:f64|crate::native_affine_grid_sign::sign(corners.map(|p|p[axis]),uv,coordinate);
   let(mut lo,mut hi)=(0,planes.len());let mut equal=None;
   while lo<hi{let mid=lo+(hi-lo)/2;match sign(planes[mid].coordinate_m())?{1=>lo=mid+1,-1=>hi=mid,0=>{equal=Some(mid);break;},_=>return Err(fail("affine grid sign is invalid"))}}
   let index=if let Some(i)=equal{if inward[axis]<0.{i.checked_sub(1)}else if inward[axis]>0.{Some(i)}else{return Err(fail("source face lies on an unresolved exact affine grid tangent incidence"));}}else{lo.checked_sub(1)};
   let Some(index)=index else{return Ok(None)};if index>=grid.shape[axis]{return Ok(None)}ijk[axis]=index;
  }
  let cell=(ijk[0]*grid.shape[1]+ijk[1])*grid.shape[2]+ijk[2];Ok(self.owner.refined.partition.native_capacity_m3.contains_key(&cell).then_some(cell))
 }
 fn classify_window_affine(&self,window:usize,uv:[f64;2],point:[f64;3],normal:[f64;3])->CaeResult<Option<SourceFaceLabel>>{
  let query=self.source_axis_query(window,uv,point)?;
  self.classify_window_with_cells_filtered(window,point,normal,|inward|self.cell_affine(window,uv,point,inward),Some(&query)).map_err(|e|fail(&format!("{e}; source_window={window} represented_uv={uv:?} actual_point={point:?} actual_normal={normal:?}")))
 }
 fn source_axis_query(&self,window:usize,uv:[f64;2],point:[f64;3])->CaeResult<SourceAxisSupportQuery<'_>>{
  let source=self.source();let facet=source.windows().get(window).ok_or_else(||fail("source support window is absent"))?.facet();
  let corners=facet.vertices.map(|p|p.map(|v|v.value()));
  if uv.iter().chain(point.iter()).any(|v|!v.is_finite())||facet.vertices.iter().flatten().any(|v|!v.finite_first_order()||v.geometry_first_order_directions().is_none()){return Err(fail("source support geometry is invalid"));}
  for corner in 0..3{if crate::native_affine_grid_sign::barycentric_sign(uv,corner)?!=1{return Err(fail("source support UV is not strictly interior"));}}
  if (0..3).any(|k|(corners[0][k]+(corners[1][k]-corners[0][k])*uv[0]+(corners[2][k]-corners[0][k])*uv[1]).to_bits()!=point[k].to_bits()){return Err(fail("source support point differs from its owned window"));}
  Ok(SourceAxisSupportQuery{refined:&self.owner.refined,corners,uv,point,sweep:None})
 }
 fn source_sweep_query<'q>(&'q self,window:usize,uv:[f64;2],point:[f64;3],receipt:&'q crate::native_union_arrangement::SweepPointReceipt)->CaeResult<SourceAxisSupportQuery<'q>>{
  receipt.validate_uv(uv).map_err(fail)?;let source=self.source();let facet=source.windows().get(window).ok_or_else(||fail("source support window is absent"))?.facet();
  let corners=facet.vertices.map(|p|p.map(|v|v.value()));
  if uv.iter().chain(point.iter()).any(|v|!v.is_finite())||facet.vertices.iter().flatten().any(|v|!v.finite_first_order()||v.geometry_first_order_directions().is_none()){return Err(fail("source support geometry is invalid"));}
  for corner in 0..3{if receipt.barycentric_sign(corner).map_err(fail)?!=1{return Err(fail("source support UV is not strictly interior"));}}
  if (0..3).any(|k|(corners[0][k]+(corners[1][k]-corners[0][k])*uv[0]+(corners[2][k]-corners[0][k])*uv[1]).to_bits()!=point[k].to_bits()){return Err(fail("source support point differs from its owned window"));}
  Ok(SourceAxisSupportQuery{refined:&self.owner.refined,corners,uv,point,sweep:Some(receipt)})
 }
 fn classify_window_sweep(&self,window:usize,uv:[f64;2],point:[f64;3],normal:[f64;3],receipt:&crate::native_union_arrangement::SweepPointReceipt)->CaeResult<Option<SourceFaceLabel>>{
 let query=self.source_sweep_query(window,uv,point,receipt)?;
 self.classify_window_with_cells_filtered(window,point,normal,|inward|self.cell_sweep(window,uv,point,inward,receipt),Some(&query))
 }
 fn classify_window_with_cells(&self,window:usize,point:[f64;3],normal:[f64;3],cell:impl Fn([f64;3])->CaeResult<Option<usize>>)->CaeResult<Option<SourceFaceLabel>>{
  self.classify_window_with_cells_filtered(window,point,normal,cell,None)
 }
 fn classify_window_with_cells_filtered(&self,window:usize,point:[f64;3],normal:[f64;3],cell:impl Fn([f64;3])->CaeResult<Option<usize>>,query:Option<&SourceAxisSupportQuery<'_>>)->CaeResult<Option<SourceFaceLabel>>{
  let source=self.owner.source;let w=source.windows().get(window).ok_or_else(||fail("source window index is out of range"))?;
  let slot=|inward|match cell(inward)?{Some(cell)=>self.index.slot_at_face_filtered(cell,point,inward,query),None=>Ok(None)};
  let minus=slot(normal.map(|x|-x))?;let plus=slot(normal)?;
  match w.kind(){SourceWindowKind::NativePhysical=>{if let Some(slot)=minus{let inward=normal.map(|x|-x);let actual_cell=cell(inward)?;let planes=self.owner.refined.partition.components.iter().enumerate().filter(|(i,c)|Some(c.cell)==actual_cell&&self.owner.component_slots[*i]==slot).map(|(i,c)|{(i,c.cell,c.volume_m3,c.convex_parts.iter().enumerate().map(|(part,p)|{(part,p.iter().map(|f|{let terms=std::array::from_fn::<_,3,_>(|k|f.outward_normal[k]*(point[k]-f.plane_origin_m[k]));let residual=terms.iter().sum::<f64>()+f.plane_offset_m;let ray=std::array::from_fn::<_,3,_>(|k|f.outward_normal[k]*inward[k]).iter().sum::<f64>();let corners=w.facet().vertices.map(|p|p.map(|v|v.value()));let corner_residuals=corners.map(|p|(0..3).map(|k|f.outward_normal[k]*(p[k]-f.plane_origin_m[k])).sum::<f64>()+f.plane_offset_m);(f.boundary.clone(),f.outward_normal,f.plane_origin_m,f.plane_offset_m,residual,ray,corner_residuals)}).collect::<Vec<_>>())}).collect::<Vec<_>>())}).collect::<Vec<_>>();return Err(fail(&format!("native physical facet has gas on its solid side slot={slot} cell={actual_cell:?} point={point:?} normal={normal:?} source={:?} support={planes:?}",w.facet()))); }Ok(plus.map(|fluid|SourceFaceLabel::NativeWall{fluid}))},SourceWindowKind::Layered{incidences}=>{
   for(side,slot)in[(1,minus),(-1,plus)]{if let Some(slot)=slot{let cell=cell(normal.map(|x|if side==1{-x}else{x}))?.ok_or_else(||fail("layered fluid cell is absent"))?;let observed=self.owner.refined.partition.components.iter().enumerate().filter(|(_,c)|c.cell==cell).filter(|(i,_)|self.owner.component_slots[*i]==slot).map(|(i,_)|self.owner.refined.regions[i]).collect::<Vec<_>>();let expected=incidences.iter().filter(|(_,s)|*s==side).map(|(r,_)|*r).collect::<Vec<_>>();let admitted=if expected.is_empty(){!observed.is_empty()&&observed.iter().all(|r|r.is_none())}else{expected.iter().any(|r|observed.contains(&Some(*r)))};if !admitted{return Err(fail("layered source fluid label disagrees with actual region incidence"));}}}
   Ok(match(minus,plus){(Some(minus),Some(plus))if minus!=plus=>Some(SourceFaceLabel::LayeredInterface{minus,plus}),_=>None})
  }}
 }
 fn cell(&self,point:[f64;3],inward:[f64;3])->CaeResult<Option<usize>>{
  if point.iter().chain(inward.iter()).any(|v|!v.is_finite())||inward.iter().all(|v|*v==0.){return Err(fail("source stage cell direction is invalid"));}
  let source=self.owner.source;let grid=&source.adapter().owner.capacity_owner().grid;let mut ijk=[0;3];
  for axis in 0..3{let planes=self.axis_planes[axis];if planes.len()!=grid.shape[axis]+1{return Err(fail("source stage grid plane coverage differs"));}let found=planes.binary_search_by(|p|p.coordinate_m().total_cmp(&point[axis]));let index=match found{Ok(i)=>{if inward[axis]<0.{i.checked_sub(1)}else if inward[axis]>0.{Some(i)}else{return Err(fail("source face lies on an unresolved grid tangent incidence"));}},Err(i)=>i.checked_sub(1)};let Some(index)=index else{return Ok(None)};if index>=grid.shape[axis]{return Ok(None)}ijk[axis]=index;}
  let cell=(ijk[0]*grid.shape[1]+ijk[1])*grid.shape[2]+ijk[2];Ok(self.owner.refined.partition.native_capacity_m3.contains_key(&cell).then_some(cell))
 }
 fn slot(&self,point:[f64;3],inward:[f64;3])->CaeResult<Option<usize>>{match self.cell(point,inward)?{Some(cell)=>self.index.slot_at_face(cell,point,inward),None=>Ok(None)}}
 fn classify_window(&self,window:usize,point:[f64;3],normal:[f64;3])->CaeResult<Option<SourceFaceLabel>>{
  let source=self.owner.source;let w=source.windows().get(window).ok_or_else(||fail("source window index is out of range"))?;
  let minus=self.slot(point,normal.map(|x|-x))?;let plus=self.slot(point,normal)?;
  match w.kind(){SourceWindowKind::NativePhysical=>{if let Some(slot)=minus{let inward=normal.map(|x|-x);let actual_cell=self.cell(point,inward)?;let planes=self.owner.refined.partition.components.iter().enumerate().filter(|(i,c)|Some(c.cell)==actual_cell&&self.owner.component_slots[*i]==slot).map(|(i,c)|{(i,c.cell,c.volume_m3,c.convex_parts.iter().enumerate().map(|(part,p)|{(part,p.iter().map(|f|{let terms=std::array::from_fn::<_,3,_>(|k|f.outward_normal[k]*(point[k]-f.plane_origin_m[k]));let residual=terms.iter().sum::<f64>()+f.plane_offset_m;let ray=std::array::from_fn::<_,3,_>(|k|f.outward_normal[k]*inward[k]).iter().sum::<f64>();let corners=w.facet().vertices.map(|p|p.map(|v|v.value()));let corner_residuals=corners.map(|p|(0..3).map(|k|f.outward_normal[k]*(p[k]-f.plane_origin_m[k])).sum::<f64>()+f.plane_offset_m);(f.boundary.clone(),f.outward_normal,f.plane_origin_m,f.plane_offset_m,residual,ray,corner_residuals)}).collect::<Vec<_>>())}).collect::<Vec<_>>())}).collect::<Vec<_>>();return Err(fail(&format!("native physical facet has gas on its solid side slot={slot} cell={actual_cell:?} point={point:?} normal={normal:?} source={:?} support={planes:?}",w.facet()))); }Ok(plus.map(|fluid|SourceFaceLabel::NativeWall{fluid}))},SourceWindowKind::Layered{incidences}=>{
   for (side,slot) in [(1,minus),(-1,plus)]{if let Some(slot)=slot{let cell=self.cell(point,normal.map(|x|if side==1{-x}else{x}))?.ok_or_else(||fail("layered fluid cell is absent"))?;let observed=self.owner.refined.partition.components.iter().enumerate().filter(|(_,c)|c.cell==cell).filter(|(i,_)|self.owner.component_slots[*i]==slot).map(|(i,_)|self.owner.refined.regions[i]).collect::<Vec<_>>();let expected=incidences.iter().filter(|(_,s)|*s==side).map(|(r,_)|*r).collect::<Vec<_>>();let admitted=if expected.is_empty(){!observed.is_empty()&&observed.iter().all(|r|r.is_none())}else{expected.iter().any(|r|observed.contains(&Some(*r)))};if !admitted{return Err(fail("layered source fluid label disagrees with actual region incidence"));}}}
   Ok(match(minus,plus){(Some(minus),Some(plus))if minus!=plus=>Some(SourceFaceLabel::LayeredInterface{minus,plus}),_=>None})
  }}
 }
}



pub(crate) struct SourceAxisSupportQuery<'a>{refined:&'a RefinedCutCellPartition<f64>,corners:[[f64;3];3],uv:[f64;2],point:[f64;3],sweep:Option<&'a crate::native_union_arrangement::SweepPointReceipt>}
impl SourceAxisSupportQuery<'_>{
 pub(crate) fn rejects(&self,refined:&RefinedCutCellPartition<f64>,point:[f64;3],part:&[crate::native_cut_cell_partition::CutPolygon<f64>])->CaeResult<bool>{
  if !std::ptr::eq(self.refined,refined)||(0..3).any(|k|self.point[k].to_bits()!=point[k].to_bits()){return Err(fail("source support query ownership differs"));}
  let corners=self.corners;let uv=self.uv;
  for face in part{
   if face.outward_normal.iter().chain(face.plane_origin_m.iter()).any(|v|!v.is_finite())||!face.plane_offset_m.is_finite(){return Err(fail("source support plane is non-finite"));}
   if face.plane_offset_m!=0.{continue;}
   let Some(axis)=(0..3).find(|&a|face.outward_normal[a].abs()==1.&&(0..3).all(|k|k==a||face.outward_normal[k]==0.))else{continue};
   let outside=if let Some(receipt)=self.sweep{receipt.affine_sign(corners.map(|p|p[axis]),face.plane_origin_m[axis]).map_err(fail)?as f64*face.outward_normal[axis]>0.}else{outside_axis(corners,uv,axis,face.outward_normal[axis],face.plane_origin_m[axis])?};if outside{return Ok(true);}
  }Ok(false)
 }
 }
fn outside_axis(corners:[[f64;3];3],uv:[f64;2],axis:usize,normal:f64,coordinate:f64)->CaeResult<bool>{
 if axis>=3||(normal!=1.&&normal!= -1.)||corners.iter().flatten().chain(uv.iter()).chain(std::iter::once(&coordinate)).any(|v|!v.is_finite()){return Err(fail("source axis halfspace is invalid"));}
 Ok(crate::native_affine_grid_sign::sign(corners.map(|p|p[axis]),uv,coordinate)? as f64*normal>0.)
}




fn wall_geometry_bits<T:TransientGasScalar>(uv:[T;2],point:[T;3],normal:[T;3])->CaeResult<Vec<u64>>{
 let mut bits=Vec::new();for v in uv.into_iter().chain(point).chain(normal){if !v.finite_first_order(){return Err(fail("source wall quadrature scalar is nonfinite"));}let directions=v.geometry_first_order_directions().ok_or_else(||fail("source wall quadrature scalar has opaque directions"))?;bits.push(v.value().to_bits());bits.push(directions.len() as u64);bits.extend(directions.into_iter().map(f64::to_bits));}Ok(bits)
}

fn source_native_parents<T:GeometryTimeBase>(source:&NativeSourceFaceWindows<'_,T>)->[std::collections::BTreeMap<[usize;3],Vec<usize>>;2]{
 let mut parents:[std::collections::BTreeMap<[usize;3],Vec<usize>>;2]=std::array::from_fn(|_|std::collections::BTreeMap::new());
 for body in 0..2{for(i,t)in source.adapter().owner.capacity_owner().elements[body].iter().enumerate(){for face in [[0,1,2],[0,1,3],[0,2,3],[1,2,3]]{let mut nodes=face.map(|k|t[k]);nodes.sort();parents[body].entry(nodes).or_default().push(i);}}}parents
}
struct NativeGridSupportAlias<'a>{axis:usize,index:usize,coordinate:f64,solid_sign:f64,grid:&'a crate::native_gap_column_owner::NativeGapColumnGrid}
fn exact_scalar_equal<S:TransientGasScalar>(a:S,b:S)->bool{
 if !a.finite_first_order()||!b.finite_first_order(){return false;}
 let d=a-b;d.value()==0.&&d.geometry_first_order_directions().is_some_and(|v|v.iter().all(|d|*d==0.))
}
impl<'a> NativeGridSupportAlias<'a>{
 fn issue<S:GeometryTimeBase>(source:&'a NativeSourceFaceWindows<'_,S>,window:usize)->CaeResult<Option<Self>>{
  let w=source.windows().get(window).ok_or_else(||fail("grid support source window is absent"))?;
  if !matches!(w.kind(),SourceWindowKind::NativePhysical){return Err(fail("grid support alias requires genuine native source"));}
  let vertices=w.facet().vertices;
  let u=std::array::from_fn::<_,3,_>(|k|vertices[1][k]-vertices[0][k]);let v=std::array::from_fn::<_,3,_>(|k|vertices[2][k]-vertices[0][k]);
  let raw=[u[1]*v[2]-u[2]*v[1],u[2]*v[0]-u[0]*v[2],u[0]*v[1]-u[1]*v[0]];
  let mut selected=None;
  for plane in source.grid_planes(){
   let axis=plane.axis();let coordinate=plane.coordinate_m();
   if !vertices.iter().all(|p|exact_scalar_equal(p[axis],crate::gas_geometry_time::GeometryTime::<S>::from_f64(coordinate))){continue;}
   if !raw[axis].finite_first_order()||raw[axis].value()==0.||!(0..3).filter(|k|*k!=axis).all(|k|exact_scalar_equal(raw[k],crate::gas_geometry_time::GeometryTime::<S>::zero())){continue;}
   if selected.is_some(){return Ok(None);}
   let grid=&source.adapter().owner.capacity_owner().grid;
   selected=Some(Self{axis,index:plane.index(),coordinate,solid_sign:if raw[axis].value()>0.{1.}else{-1.},grid});
  }Ok(selected)
 }
 fn matches<S:TransientGasScalar>(&self,plane:&crate::native_cut_cell_partition::CutPolygon<S>,bounds:[[f64;3];2])->bool{
  let crate::native_cut_cell_partition::CutBoundary::Cell{axis,side}=plane.boundary else{return false;};
  axis==self.axis&&side<2&&(self.grid.origin_m[axis]+self.index as f64*self.grid.spacing_m).to_bits()==self.coordinate.to_bits()&&bounds[side][axis].to_bits()==self.coordinate.to_bits()
   &&exact_scalar_equal(plane.plane_origin_m[axis],S::from_f64(self.coordinate))&&exact_scalar_equal(plane.plane_offset_m,S::zero())
   &&(0..3).all(|k|exact_scalar_equal(plane.outward_normal[k],S::from_f64(if k==axis{-self.solid_sign}else{0.})))
 }
 fn normal_matches<S:TransientGasScalar>(&self,normal:[S;3])->bool{(0..3).all(|k|exact_scalar_equal(normal[k],S::from_f64(if k==self.axis{self.solid_sign}else{0.})))}
}
pub(crate) struct SourceNativeFacetIdentity<'a>{refined:&'a RefinedCutCellPartition<f64>,body:usize,element:usize,face:Option<[usize;3]>,grid_alias:Option<NativeGridSupportAlias<'a>>}
impl SourceNativeFacetIdentity<'_>{
 fn validate_emitted_support<S:TransientGasScalar>(&self,refined:&RefinedCutCellPartition<f64>,cell:usize,component:usize,part:usize,plane:usize,normal:[S;3])->CaeResult<()>{
  if !std::ptr::eq(self.refined,refined){return Err(fail("source emitted support partition differs"));}
  let c=refined.partition.components.get(component).ok_or_else(||fail("source emitted component is absent"))?;
  if c.cell!=cell{return Err(fail("source emitted support cell differs"));}
  let p=c.convex_parts.get(part).and_then(|p|p.get(plane)).ok_or_else(||fail("source emitted support plane is absent"))?;
  if matches!(p.boundary,crate::native_cut_cell_partition::CutBoundary::Cell{..}){
   let alias=self.grid_alias.as_ref().ok_or_else(||fail("source emitted Cell support lacks native grid authority"))?;
   if !alias.matches(p,alias.grid.cell_bounds(cell)?)||!alias.normal_matches(normal){return Err(fail("source emitted grid support normal full channels differ"));}
  }Ok(())
 }
 pub(crate) fn support_plane_in_cell(&self,refined:&RefinedCutCellPartition<f64>,cell:usize,part:&[crate::native_cut_cell_partition::CutPolygon<f64>])->CaeResult<Option<usize>>{
  if let Some(native)=self.support_plane(refined,part)?{return Ok(Some(native));}
  let Some(alias)=&self.grid_alias else{return Ok(None);};let bounds=alias.grid.cell_bounds(cell)?;
  if refined.partition.apertures.iter().any(|a|a.axis==Some(alias.axis)&&a.right.is_none()&&refined.partition.components.get(a.left).is_some_and(|c|c.cell==cell)&&a.normal[alias.axis]*(-alias.solid_sign)>0.){return Err(fail("native grid alias conflicts with declared exterior grid ownership"));}
  if !std::ptr::eq(self.refined,refined){return Err(fail("native grid support partition differs"));}
  let mut selected=None;
  for(i,plane)in part.iter().enumerate(){if alias.matches(plane,bounds){if selected.replace(i).is_some(){return Err(fail("native grid supporting plane is ambiguous"));}}}
  Ok(selected)
 }
 pub(crate) fn support_plane(&self,refined:&RefinedCutCellPartition<f64>,part:&[crate::native_cut_cell_partition::CutPolygon<f64>])->CaeResult<Option<usize>>{
  if !std::ptr::eq(self.refined,refined){return Err(fail("native support identity partition differs"));}
  let mut selected:Option<usize>=None;
  for(i,p)in part.iter().enumerate(){let crate::native_cut_cell_partition::CutBoundary::Native{body,element,face,physical:true}=p.boundary else{continue};let mut actual=face;if let Some(ref mut nodes)=actual{nodes.sort();}if body!=self.body||element!=self.element||actual!=self.face{continue;}
   if let Some(previous)=selected{let first:&crate::native_cut_cell_partition::CutPolygon<f64>=&part[previous];if first.outward_normal.map(f64::to_bits)!=p.outward_normal.map(f64::to_bits)||first.plane_origin_m.map(f64::to_bits)!=p.plane_origin_m.map(f64::to_bits)||first.plane_offset_m.to_bits()!=p.plane_offset_m.to_bits(){return Err(fail("native support plane identity has conflicting coefficients"));}}
   else{selected=Some(i);}
  }Ok(selected)
 }
}
