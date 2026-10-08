// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::collections::BTreeMap;
use implexity_ad::Scalar;
use implexity_physics_thermofluid::rv::{Recording,Rv};
use crate::errors::PResult;
fn record<F:Fn(&[Rv],&BTreeMap<String,Vec<Rv>>)->Vec<Rv>>(topology:&[f64],phases:&BTreeMap<String,Vec<f64>>,rec:&Recording,response:&F)->(Vec<Rv>,BTreeMap<String,Vec<Rv>>,Vec<Rv>){
 let s=rec.inputs(topology);
 let q=phases.iter().map(|(k,v)|(k.clone(),rec.inputs(v))).collect();
 let out=response(&s,&q);
 (s,q,out)
}
pub type ResponseRows=(Vec<f64>,Vec<(Vec<f64>,BTreeMap<String,Vec<f64>>)>);
pub fn jacobian<F:Fn(&[Rv],&BTreeMap<String,Vec<Rv>>)->Vec<Rv>>(topology:&[f64],phases:&BTreeMap<String,Vec<f64>>,indices:&[usize],response:F)->PResult<ResponseRows>{
 let rec=Recording::start()?;
 let (s,q,out)=record(topology,phases,&rec,&response);
 let mut inputs=s.clone();
 let mut layout=Vec::new();
 for (name,vars) in &q{layout.push((name.clone(),inputs.len(),vars.len()));inputs.extend(vars.iter().copied());}
 let mut values=Vec::new();let mut rows=Vec::new();
 for idx in indices{let g_all=rec.vjp(&[out[*idx]],&[1.0],&inputs);values.push(out[*idx].value());rows.push((g_all[..s.len()].to_vec(),layout.iter().map(|(name,start,len)|(name.clone(),g_all[*start..start+len].to_vec())).collect()));}
 Ok((values,rows))
}
pub type WeightedRow=(Vec<f64>,f64,Vec<f64>,BTreeMap<String,Vec<f64>>);
pub fn vjp<F:Fn(&[Rv],&BTreeMap<String,Vec<Rv>>)->Vec<Rv>>(topology:&[f64],phases:&BTreeMap<String,Vec<f64>>,weights:&[f64],response:F)->PResult<WeightedRow>{
 let rec=Recording::start()?;
 let (s,q,out)=record(topology,phases,&rec,&response);
 let topology_gradient=rec.vjp(&out,weights,&s);
 let phase_gradients=q.iter().map(|(name,vars)|(name.clone(),rec.vjp(&out,weights,vars))).collect();
 let values:Vec<f64>=out.iter().map(Scalar::value).collect();
 let value=weights.iter().zip(&values).map(|(w,v)|w*v).sum();
 Ok((values,value,topology_gradient,phase_gradients))
}
