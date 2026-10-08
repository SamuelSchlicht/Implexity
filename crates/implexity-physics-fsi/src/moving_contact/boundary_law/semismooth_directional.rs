// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum ContactDifferentialKind{OrdinaryAwayFromOrigin,HadamardDirectionalAtOrigin}
#[derive(Clone,Copy,Debug)]
pub struct ContactResidualDirection{pub value:f64,pub kind:ContactDifferentialKind}
pub fn fischer_burmeister_direction(gap:f64,multiplier:f64,gap_scale:f64,force_scale:f64,gap_direction:f64,multiplier_direction:f64)->CaeResult<ContactResidualDirection>{
 if ![gap,multiplier,gap_scale,force_scale,gap_direction,multiplier_direction].iter().all(|v|v.is_finite())||gap_scale<=0.||force_scale<=0.{return Err(CaeError::contract("directional complementarity input"));}
 let a=gap/gap_scale;let b=multiplier/force_scale;let da=gap_direction/gap_scale;let db=multiplier_direction/force_scale;let radius=a.hypot(b);
 let (value,kind)=if radius==0.{(da.hypot(db)-da-db,ContactDifferentialKind::HadamardDirectionalAtOrigin)}else{((a/radius-1.)*da+(b/radius-1.)*db,ContactDifferentialKind::OrdinaryAwayFromOrigin)};
 if ![a,b,da,db,radius,value].iter().all(|v|v.is_finite()){return Err(CaeError::contract("directional complementarity overflow"));}Ok(ContactResidualDirection{value,kind})
}
pub fn origin_ray_b_subgradient(gap_scale:f64,force_scale:f64,gap_direction:f64,multiplier_direction:f64)->CaeResult<[f64;2]>{
 if ![gap_scale,force_scale,gap_direction,multiplier_direction].iter().all(|v|v.is_finite())||gap_scale<=0.||force_scale<=0.{return Err(CaeError::contract("contact ray input"));}let a=gap_direction/gap_scale;let b=multiplier_direction/force_scale;let r=a.hypot(b);if !r.is_finite()||r==0.{return Err(CaeError::contract("nonzero normalized contact ray required"));}let out=[(a/r-1.)/gap_scale,(b/r-1.)/force_scale];if out.iter().any(|v|!v.is_finite()){return Err(CaeError::contract("contact ray differential overflow"));}Ok(out)
}
