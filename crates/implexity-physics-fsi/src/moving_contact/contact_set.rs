// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::multirate_coupling::FluxDrivenField;
use crate::moving_contact::{model::MovingContactSpec,separate_body::{MovingFsiPairModel,PairSolidField},mapped_pair_law::{MappedPairLaw,NodeBinding},contact_field::ContactField};
use crate::moving_contact::collection::MultipleContact;
fn fail(s:impl std::fmt::Display)->CaeError{CaeError::contract(s.to_string())}
pub fn multiple_contact_field<'a>(model:&'a MovingFsiPairModel,specs:&[MovingContactSpec])->CaeResult<ContactField<PairSolidField<'a>,MultipleContact<MappedPairLaw>>>{let solid=model.solid_field()?;let law=multiple_contact_law(model,&solid,specs)?;ContactField::new(solid,law)}
pub fn multiple_contact_law(model:&MovingFsiPairModel,solid:&PairSolidField<'_>,specs:&[MovingContactSpec])->CaeResult<MultipleContact<MappedPairLaw>>{
 if specs.is_empty(){return Err(fail("contact set requires features"));}let mut laws=vec![];
 for spec in specs{if !spec.replaced_planes.is_empty(){return Err(fail("contact set cannot replace external planes"));}let mut nodes:[Vec<NodeBinding>;2]=std::array::from_fn(|_|vec![]);let(mut ri,mut ci,mut vs)=(vec![],vec![],vec![]);let mut row=0;let(mut offset_state,mut offset_flux,mut offset_design)=(0,0,0);
  for b in 0..2{let body=model.native_body(b)?;let grid=&body.problem.solid.grid;let field=solid.body_field(b)?;let scale=field.core().scale();let mut seen=std::collections::BTreeSet::new();if spec.body_nodes[b].is_empty()||spec.body_nodes[b].iter().any(|i|*i>=grid.node_count()||!seen.insert(*i)){return Err(fail("contact set native node selection"));}
   for &id in &spec.body_nodes[b]{let mut state=[0;3];let mut inverse=[0.;3];for a in 0..3{let(c,v)=field.trace_operator().row(3*id+a);if c.len()!=1||v!=[1.]{return Err(fail("contact set native nodal trace"));}state[a]=offset_state+c[0];inverse[a]=1./scale[c[0]];}nodes[b].push(NodeBinding{reference_m:grid.node_position(id),state,inverse_state_scale:inverse,force_flux:[offset_flux+3*id,offset_flux+3*id+1,offset_flux+3*id+2]});let ijk=grid.node_ijk(id);let mut incident=vec![];for x in 0..2{for y in 0..2{for z in 0..2{let s=[x,y,z];let mut v=[0;3];let mut ok=true;for a in 0..3{if ijk[a]<s[a]{ok=false;break;}v[a]=ijk[a]-s[a];if v[a]>=grid.shape[a]{ok=false;break;}}if ok{incident.push(grid.voxel_index(v));}}}}if incident.is_empty(){return Err(fail("contact set incident phase"));}let weight=1./incident.len()as f64;for v in incident{ri.push(row);ci.push(offset_design+v);vs.push(weight);}row+=1;}
   offset_state+=field.state_size();offset_flux+=field.trace_operator().nrows();offset_design+=field.design_size();
  }
  let phase=CsrMatrix::from_triplets(row,solid.design_size(),&ri,&ci,&vs).map_err(fail)?;laws.push(MappedPairLaw::new(nodes,spec.features.clone(),spec.bodies,phase,solid.state_size(),solid.trace_operator().nrows(),spec.gap_scale_m,spec.force_scale_n)?.with_path_policy(spec.path_policy)?);
 }
 MultipleContact::new(laws,solid.state_size(),solid.trace_operator().nrows(),solid.design_size())
}

pub fn native_velocity_trace(model:&MovingFsiPairModel,design:&[f64],time_scale:f64,multipliers:usize)->CaeResult<CsrMatrix>{let pair=model.solid_field()?;native_velocity_trace_from_pair(model,&pair,design,time_scale,multipliers)}
pub fn native_velocity_trace_from_pair(model:&MovingFsiPairModel,pair:&PairSolidField<'_>,design:&[f64],time_scale:f64,multipliers:usize)->CaeResult<CsrMatrix>{
 if !time_scale.is_finite()||time_scale<=0.||multipliers==0{return Err(fail("native contact velocity clock/layout"));}if design.len()!=pair.design_size(){return Err(fail("native contact velocity design shape"));}let(mut ri,mut ci,mut values)=(vec![],vec![],vec![]);let(mut row_offset,mut state_offset,mut design_offset)=(0,0,0);
 for b in 0..2{let body=model.native_body(b)?;let field=pair.body_field(b)?;let nd=field.design_size();let core=field.core();let history=core.history(implexity_solve::time_stepper::StepParameters{design:&design[design_offset..design_offset+nd],time_scale})?;if history.layout.n3!=body.problem.solid.grid.node_count()*3{return Err(fail("native contact velocity node layout"));}for i in 0..history.layout.n3{let local=history.layout.v()+i;let scale=core.scale()[local];if !scale.is_finite()||scale<=0.{return Err(fail("native contact velocity scale"));}ri.push(row_offset+i);ci.push(state_offset+local);values.push(1./scale);}row_offset+=field.trace_operator().nrows();state_offset+=field.state_size();design_offset+=nd;}
 CsrMatrix::from_triplets(row_offset,state_offset+multipliers,&ri,&ci,&values).map_err(fail)
}
