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

use crate::native_cut_cell_refinement::CutTetraSubdomain;
use crate::gas_gap_mass::GasMassScalar;
use crate::gas_geometry_time::{GeometryTime,GeometryTimeBase};
use crate::gas_transient_kinetic::TransientGasScalar;
use crate::gas_layered_tetra_mesh::{self,LayeredGapTriangle};
use crate::native_closed_stage::ClosedStage;
use crate::native_gas_geometry_adapter::NativeGasGeometryAdapter;
use crate::native_union_boundary::PhysicalFacet;
use crate::native_union_facets::authentic_native_facets;
use implexity_ad::Scalar;
use implexity_core::{CaeError,CaeResult};
use std::collections::BTreeMap;
pub(crate) mod column_uv;
pub(crate) mod parallelogram_uv;
fn fail(s:&str)->CaeError{CaeError::contract(s)}
fn same<S:TransientGasScalar>(a:S,b:S)->bool{let d=a-b;d.value()==0.&&d.geometry_first_order_directions().is_some_and(|v|v.iter().all(|x|x.is_finite()&&*x==0.))}
fn cross<S:Scalar>(a:[S;3],b:[S;3])->[S;3]{[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]]}
fn sub<S:Scalar>(a:[S;3],b:[S;3])->[S;3]{std::array::from_fn(|k|a[k]-b[k])}
fn dot<S:Scalar>(a:[S;3],b:[S;3])->S{(0..3).fold(S::zero(),|s,k|s+a[k]*b[k])}
#[derive(Clone,Debug,PartialEq,Eq)]
pub(crate) enum SourceWindowKind{NativePhysical,Layered{incidences:Vec<(usize,i8)>}}
pub(crate) struct SourceFaceWindow<S:TransientGasScalar>{facet:PhysicalFacet<S>,kind:SourceWindowKind}
impl<S:TransientGasScalar> SourceFaceWindow<S>{pub(crate) fn facet(&self)->&PhysicalFacet<S>{&self.facet}pub(crate) fn kind(&self)->&SourceWindowKind{&self.kind}}
pub(crate) struct FixedGridPlane{axis:usize,index:usize,coordinate_m:f64}
impl FixedGridPlane{pub(crate) fn axis(&self)->usize{self.axis}pub(crate) fn index(&self)->usize{self.index}pub(crate) fn identity(&self)->[usize;2]{[self.axis,self.index]}pub(crate) fn coordinate_m(&self)->f64{self.coordinate_m}}
mod rational_four_column;
mod layer_quotient_owner;
pub(crate) struct NativeSourceFaceWindows<'g,S:GeometryTimeBase>{adapter:&'g NativeGasGeometryAdapter,stage_trace:Vec<f64>,stage_design:Vec<f64>,live_trace:Vec<GeometryTime<S>>,live_design:Vec<GeometryTime<S>>,stage_tets:Vec<CutTetraSubdomain<GeometryTime<S>>>,windows:Vec<SourceFaceWindow<GeometryTime<S>>>,grid_planes:Vec<FixedGridPlane>,stage_fraction:f64,mapped_columns:Vec<[[GeometryTime<S>;3];6]>,maintained_charts:std::collections::BTreeSet<usize>,trace_recipe_charts:std::collections::BTreeSet<usize>,corner_correspondence:std::cell::RefCell<std::collections::BTreeMap<usize,Vec<super_correspondence::CoordinateCorrespondence>>>,facet_owner_index:BTreeMap<(Vec<usize>,[Vec<usize>;3]),usize>,subdomain_windows:BTreeMap<(usize,usize),usize>,layer_quotients:Option<layer_quotient_owner::LayerQuotientOwner>}
impl<'g,S:GeometryTimeBase> NativeSourceFaceWindows<'g,S>{
 pub(crate) fn from_stage(adapter:&'g NativeGasGeometryAdapter,start:&[S],end:&[S],design:&[S],step_s:S,time_scale:f64,alpha:f64,closed:Option<ClosedStage<'_>>)->CaeResult<Self>{
  if !step_s.finite_first_order()||step_s.value()<=0.||!alpha.is_finite()||!(0. ..=1.).contains(&alpha)||start.len()!=end.len()||start.iter().chain(end).chain(design).any(|v|!v.finite_first_order()){return Err(fail("source face stage inputs are invalid"));}
  let owner=adapter.owner.capacity_owner();adapter.kinematics.validate_native_owner_bindings(owner)?;let trace_recipe_charts=adapter.kinematics.native_chart_trace_recipe_charts();
  let a=start.iter().map(|v|v.value()).collect::<Vec<_>>();let z=end.iter().map(|v|v.value()).collect::<Vec<_>>();let x=design.iter().map(|v|v.value()).collect::<Vec<_>>();
  let selected=match closed{None=>None,Some(ClosedStage::Adapter(owner))=>Some(owner.selected_charts(adapter,start,end,design,step_s,time_scale)?),Some(ClosedStage::Witness(w))=>{w.validate_path(&adapter.kinematics,&a,&z,&x)?;w.validate_time_scale(time_scale)?;Some((w.closed_start_charts().to_vec(),w.closed_end_charts().to_vec()))},Some(ClosedStage::Release{sample,macro_start,macro_end})=>{sample.validate_path(&adapter.kinematics,macro_start,macro_end,&a,&z,&x)?;sample.parent().validate_time_scale(time_scale)?;Some((sample.closed_start_charts().to_vec(),sample.closed_end_charts().to_vec()))},Some(ClosedStage::Trial{sample,macro_start,macro_end})=>{sample.validate_path(&adapter.kinematics,macro_start,macro_end,&a,&z,&x)?;sample.parent().validate_time_scale(time_scale)?;Some((sample.closed_start_charts().to_vec(),sample.closed_end_charts().to_vec()))}};
  let trace=start.iter().zip(end).map(|(&a,&b)|GeometryTime::new(a+(b-a)*alpha,(b-a)/step_s)).collect::<Vec<_>>();let d=design.iter().map(|&v|GeometryTime::new(v,S::zero())).collect::<Vec<_>>();
  let mapped=adapter.kinematics.mapped_geometry_from_trace(&trace,&d,|v,_|Ok(v))?;
  if mapped.len()!=adapter.paired_vertex_ids.len(){return Err(fail("source layered chart shape differs"));}
  let mut features=BTreeMap::<String,usize>::new();let mut ids=BTreeMap::<usize,String>::new();for (chart,pair) in adapter.paired_vertex_ids.iter().enumerate(){let f=adapter.kinematics.chart_features(chart)?;for j in 0..3{let key=serde_json::to_string(&[&f[0][j],&f[1][j]]).map_err(|_|fail("source chart feature cannot serialize"))?;if features.get(&key).is_some_and(|i|*i!=pair[j])||ids.get(&pair[j]).is_some_and(|k|k!=&key){return Err(fail("source paired vertex identities disagree with actual contact features"));}features.insert(key.clone(),pair[j]);ids.insert(pair[j],key);}}
  let triangles=mapped.iter().zip(&adapter.paired_vertex_ids).map(|(&p,&ids)|LayeredGapTriangle{paired_vertex_ids:ids,start_m:p,end_m:p}).collect::<Vec<_>>();
  let maintained_charts=selected.as_ref().map(|(a,b)|a.iter().copied().filter(|c|if alpha==0.{true}else{b.contains(c)}).chain(if alpha==1.{b.clone()}else{Vec::new()}).collect()).unwrap_or_default();
  let mesh=if let Some((a,b))=selected{gas_layered_tetra_mesh::build_closed_endpoint(&triangles,adapter.layers,GeometryTime::<S>::one(),adapter.length_tolerance_m,&a,&b,alpha)?}else{gas_layered_tetra_mesh::build(&triangles,adapter.layers,GeometryTime::<S>::one(),adapter.length_tolerance_m)?};
  let layer_quotients=layer_quotient_owner::LayerQuotientOwner::issue(&mesh,&mapped,&adapter.paired_vertex_ids,adapter.layers).ok();
  let native=authentic_native_facets(owner,&trace,&d,same).map_err(|e|fail(&e))?;let mut windows=native.into_iter().map(|facet|SourceFaceWindow{facet,kind:SourceWindowKind::NativePhysical}).collect::<Vec<_>>();
  let mut stage_tets=Vec::new();let mut subdomain_keys=BTreeMap::new();
  let mut faces=BTreeMap::<[Vec<usize>;3],(PhysicalFacet<GeometryTime<S>>,Vec<(usize,i8)>)>::new();
  for t in &mesh.tetrahedra{if t.volume_m3.value()==0.{if !same(t.volume_m3,GeometryTime::<S>::zero()){return Err(fail("source zero layered tetra has unresolved jets"));}continue;}let region=(t.chart*adapter.layers+t.layer)*3+t.local_tetra;stage_tets.push(CutTetraSubdomain{region,vertex_ids:Some(t.nodes),vertices_m:t.nodes.map(|i|mesh.nodes[i].end_m)});
   for (face,f) in [[0,1,2,3],[0,1,3,2],[0,2,3,1],[1,2,3,0]].into_iter().enumerate(){let mut local=[f[0],f[1],f[2]];let mut points=local.map(|j|mesh.nodes[t.nodes[j]].end_m);let n=cross(sub(points[1],points[0]),sub(points[2],points[0]));let toward=dot(n,sub(mesh.nodes[t.nodes[f[3]]].end_m,points[0]));if toward.value()==0.||!toward.finite_first_order(){return Err(fail("source layered face loses tetrahedral rank"));}if toward.value()>0.{local.swap(1,2);points.swap(1,2);}let vertex_features=local.map(|j|{let n=&mesh.nodes[t.nodes[j]];vec![2,n.paired_vertex,n.layer]});let mut key=vertex_features.clone();key.sort();subdomain_keys.insert((region,face),key.clone());
    if let Some((prior,regions))=faces.get_mut(&key){if regions.len()!=1||regions.iter().any(|(r,_)|*r==region){return Err(fail("source layered face has invalid incidence multiplicity"));}for (i,p) in points.iter().enumerate(){let j=prior.vertex_features.iter().position(|f|*f==vertex_features[i]).ok_or_else(||fail("source layered shared vertex is absent"))?;if(0..3).any(|k|!same(p[k],prior.vertices[j][k])){return Err(fail("source layered shared face has unequal full geometry"));}}let a=cross(sub(prior.vertices[1],prior.vertices[0]),sub(prior.vertices[2],prior.vertices[0]));let b=cross(sub(points[1],points[0]),sub(points[2],points[0]));if dot(a,b).value()>=0.{return Err(fail("source layered shared face is not oppositely oriented"));}regions.push((region,-1));
    }else{let mut owner=vec![2];for p in &key{owner.push(p.len());owner.extend(p);}faces.insert(key,(PhysicalFacet{owner,vertices:points,vertex_features},vec![(region,1)]));}
   }
  }
  let mut layered_keys=BTreeMap::new();
  for (key, (facet,mut regions)) in faces{regions.sort_unstable();layered_keys.insert(key,windows.len());windows.push(SourceFaceWindow{facet,kind:SourceWindowKind::Layered{incidences:regions}});}
  let subdomain_windows=subdomain_keys.into_iter().map(|(owner,key)|layered_keys.get(&key).copied().map(|window|(owner,window)).ok_or_else(||fail("source subdomain face window is absent"))).collect::<CaeResult<BTreeMap<_,_>>>()?;
  let mut grid_planes=Vec::new();for axis in 0..3{for index in 0..=owner.grid.shape[axis]{let coordinate_m=owner.grid.origin_m[axis]+index as f64*owner.grid.spacing_m;if !coordinate_m.is_finite(){return Err(fail("source grid plane coordinate overflow"));}grid_planes.push(FixedGridPlane{axis,index,coordinate_m});}}
  let mut facet_owner_index=BTreeMap::new();for (i,w) in windows.iter().enumerate(){if facet_owner_index.insert((w.facet.owner.clone(),w.facet.vertex_features.clone()),i).is_some(){return Err(fail("source physical facet owner is duplicated"));}}
  Ok(Self{adapter,stage_trace:trace.iter().map(|v|v.value()).collect(),stage_design:x,live_trace:trace,live_design:d,stage_tets,windows,grid_planes,stage_fraction:alpha,mapped_columns:mapped,maintained_charts,trace_recipe_charts,corner_correspondence:std::cell::RefCell::new(std::collections::BTreeMap::new()),facet_owner_index,subdomain_windows,layer_quotients})
 }
 pub(crate) fn layered_window_for_subdomain(&self,region:usize,face:usize)->Option<usize>{self.subdomain_windows.get(&(region,face)).copied()}
 pub(crate) fn layered_window_is_fixed_grid_owned(&self,window:usize)->CaeResult<bool>{
  let window=self.windows.get(window).ok_or_else(||fail("source layered grid window is absent"))?;
  if !matches!(window.kind,SourceWindowKind::Layered{..}){return Err(fail("source grid plane owner is not layered"));}
  Ok(self.grid_planes.iter().any(|plane|window.facet.vertices.iter().all(|point|same(point[plane.axis],GeometryTime::<S>::from_f64(plane.coordinate_m)))))
 }
 pub(crate) fn adapter(&self)->&'g NativeGasGeometryAdapter{self.adapter}
 pub(crate) fn stage_trace(&self)->&[f64]{&self.stage_trace}
 pub(crate) fn stage_design(&self)->&[f64]{&self.stage_design}
 pub(crate) fn live_stage_trace(&self)->&[GeometryTime<S>]{&self.live_trace}
 pub(crate) fn live_stage_design(&self)->&[GeometryTime<S>]{&self.live_design}
 pub(crate) fn stage_tets(&self)->&[CutTetraSubdomain<GeometryTime<S>>]{&self.stage_tets}
 pub(crate) fn windows(&self)->&[SourceFaceWindow<GeometryTime<S>>]{&self.windows}
 pub(crate) fn grid_planes(&self)->&[FixedGridPlane]{&self.grid_planes}
 pub(crate) fn stage_fraction(&self)->f64{self.stage_fraction}
}

pub(crate) mod endpoint_alias;



pub(crate) mod affine_corner;

pub(crate) mod reference_fibre;

pub(crate) mod native_closed_corner;

pub(crate) mod super_correspondence;

pub(crate) mod present_primal_corner;
