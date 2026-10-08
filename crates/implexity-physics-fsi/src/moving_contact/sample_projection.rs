// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_solve::dynamic_program::{self,DynamicProgram};
use serde_json::Value;
#[derive(Clone,Debug)]
pub struct PairSampleProjection{source_names:Vec<String>,names:Vec<String>,rows:Vec<Vec<(usize,f64)>>,offsets:Vec<f64>}
impl PairSampleProjection{
 pub fn new(source_names:Vec<String>,names:Vec<String>,rows:Vec<Vec<(usize,f64)>>,offsets:Vec<f64>)->CaeResult<Self>{
  if source_names.is_empty()||names.is_empty()||rows.len()!=names.len()||offsets.len()!=names.len()||offsets.iter().any(|v|!v.is_finite())||source_names.iter().any(|v|v.is_empty())||names.iter().any(|v|v.is_empty())||source_names.iter().enumerate().any(|(i,v)|source_names[..i].contains(v))||names.iter().enumerate().any(|(i,v)|names[..i].contains(v))||rows.iter().any(|r|r.is_empty()||r.iter().enumerate().any(|(i,(j,v))|*j>=source_names.len()||!v.is_finite()||r[..i].iter().any(|(k,_)|k==j))){return Err(CaeError::contract("pair sample projection domain"));}Ok(Self{source_names,names,rows,offsets})
 }
 pub fn source_names(&self)->&[String]{&self.source_names}
 pub fn names(&self)->&[String]{&self.names}
 pub fn values(&self,source:&[f64])->CaeResult<Vec<f64>>{if source.len()!=self.source_names.len()||source.iter().any(|v|!v.is_finite()){return Err(CaeError::contract("pair sample source values"));}let out:Vec<_>=self.rows.iter().zip(&self.offsets).map(|(r,o)|r.iter().fold(*o,|v,(i,a)|v+a*source[*i])).collect();if out.iter().any(|v|!v.is_finite()){return Err(CaeError::contract("pair sample projection overflow"));}Ok(out)}
 pub fn pullback(&self,bar:&[f64])->CaeResult<Vec<f64>>{if bar.len()!=self.names.len()||bar.iter().any(|v|!v.is_finite()){return Err(CaeError::contract("pair sample cotangent shape"));}let mut out=vec![0.;self.source_names.len()];for(r,b)in self.rows.iter().zip(bar){for(i,a)in r{out[*i]+=a*b;}}if out.iter().any(|v|!v.is_finite()){return Err(CaeError::contract("pair sample cotangent overflow"));}Ok(out)}
 pub fn bind_program(&self,program:&Value,design_kinds:&[&str],rows:usize,periodic:bool,autonomous:bool)->CaeResult<DynamicProgram>{let p=dynamic_program::normalise(program)?.bind(&self.names,design_kinds)?;p.admit(rows,periodic,autonomous)?;Ok(p)}
 pub fn to_value(&self)->Value{serde_json::json!({"source_names":self.source_names,"names":self.names,"rows":self.rows,"offsets":self.offsets})}
}

pub fn from_observables(original:&Value,bodies:[&Value;2],source_names:Vec<String>)->CaeResult<PairSampleProjection>{
 let list=original.as_array().ok_or_else(||CaeError::contract("pair original observables"))?;let mut names=vec![];let mut rows=vec![];let mut offsets=vec![];
 let index=|name:&str|->CaeResult<usize>{source_names.iter().position(|s|s==name).ok_or_else(||CaeError::contract(format!("pair sample missing {name}")))};
 let routed=|obs:&Value|->CaeResult<usize>{let name=obs["name"].as_str().ok_or_else(||CaeError::contract("pair sample name"))?;let mut found=vec![];for b in 0..2{let list=bodies[b].as_array().ok_or_else(||CaeError::contract("pair body observables"))?;if list.iter().any(|v|v==obs){if let Ok(i)=index(&format!("body{b}:{name}")){found.push(i);}}}if found.len()!=1{return Err(CaeError::contract("pair solid sample must have one body owner"));}Ok(found[0])};
 for obs in list{let name=obs["name"].as_str().ok_or_else(||CaeError::contract("pair sample name"))?;let kind=obs["kind"].as_str().ok_or_else(||CaeError::contract("pair sample kind"))?;let mut offset=0.;let row=if kind=="probe_separation"{let a=&obs["point_a_m"];let b=&obs["point_b_m"];let c=obs["component"].as_u64().ok_or_else(||CaeError::contract("pair separation component"))?as usize;if c>=3{return Err(CaeError::contract("pair separation component"));}let probe=|point:&Value|->CaeResult<usize>{let v=list.iter().find(|v|v["kind"]=="probe_displacement"&&v["point_m"]==*point&&v["component"]==obs["component"]).ok_or_else(||CaeError::contract("pair separation needs explicit native endpoint probes"))?;routed(v)};let ia=probe(a)?;let ib=probe(b)?;if ia==ib{return Err(CaeError::contract("pair separation endpoint owners"));}offset=b[c].as_f64().ok_or_else(||CaeError::contract("pair separation reference"))?-a[c].as_f64().ok_or_else(||CaeError::contract("pair separation reference"))?;vec![(ia,-1.),(ib,1.)]}else if let Ok(i)=index(name){vec![(i,1.)]}else{vec![(routed(obs)?,1.)]};names.push(name.to_string());rows.push(row);offsets.push(offset);}
 PairSampleProjection::new(source_names,names,rows,offsets)
}
