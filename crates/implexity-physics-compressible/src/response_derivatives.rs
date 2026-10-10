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
