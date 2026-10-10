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

use implexity_core::{CaeError,CaeResult};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
fn sub(a:[f64;3],b:[f64;3])->[f64;3]{std::array::from_fn(|i|a[i]-b[i])}
fn cross(a:[f64;3],b:[f64;3])->[f64;3]{[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]]}
#[derive(Clone,Debug)]pub struct SweptCandidate{pub facets:[usize;2],pub vertex_face:[([usize;2],usize);6],pub edge_edge:[[[usize;2];2];9]}
#[derive(Clone,Debug)]pub struct SweptCoverage{pub examined_pairs:usize,pub aabb_separated:usize,pub exact_axis_separated:usize,pub unresolved:Vec<SweptCandidate>,pub endpoint_identity:String}
impl SweptCoverage{pub fn require_swept_boundary_separated(&self)->CaeResult<()>{if !self.unresolved.is_empty(){return Err(fail("global swept surface candidate requires narrow contact/stratum owner; no separated-boundary admission"));}Ok(())}}
pub fn exhaustive_swept_candidates(old:[&[[f64;3]];2],new:[&[[f64;3]];2],facets:[&[[usize;3]];2],maximum_pairs:usize)->CaeResult<SweptCoverage>{
 if facets.iter().any(|f|f.is_empty())||maximum_pairs==0||old.iter().zip(new).any(|(a,b)|a.len()!=b.len()||a.is_empty())||old.iter().chain(new.iter()).flat_map(|a|a.iter().flatten()).any(|x|!x.is_finite()){return Err(fail("global swept surface endpoints/budget"));}let pairs=facets[0].len().checked_mul(facets[1].len()).ok_or_else(||fail("global swept pair overflow"))?;if pairs>maximum_pairs{return Err(fail("global swept coverage budget exhausted"));}for b in 0..2{for f in facets[b]{if f.iter().any(|i|*i>=old[b].len())||f[0]==f[1]||f[1]==f[2]||f[0]==f[2]{return Err(fail("global swept facet connectivity"));}}}
 let bounds: [Vec<[[f64;3];2]>;2]=std::array::from_fn(|b|facets[b].iter().map(|f|std::array::from_fn(|side|std::array::from_fn(|k|f.iter().flat_map(|i|[old[b][*i][k],new[b][*i][k]]).fold(if side==0{f64::INFINITY}else{f64::NEG_INFINITY},|a,v|if side==0{a.min(v)}else{a.max(v)})))).collect());
 let mut report=SweptCoverage{examined_pairs:pairs,aabb_separated:0,exact_axis_separated:0,unresolved:vec![],endpoint_identity:implexity_core::json::canonical_sha256(&serde_json::json!({"old":old,"new":new,"facets":facets}))};
 for (i,f)in facets[0].iter().enumerate(){for (j,g)in facets[1].iter().enumerate(){if (0..3).any(|k|bounds[0][i][1][k]<bounds[1][j][0][k]||bounds[1][j][1][k]<bounds[0][i][0][k]){report.aabb_separated+=1;continue;}let a:[[[f64;3];3];2]=[f.map(|i|old[0][i]),f.map(|i|new[0][i])];let b:[[[f64;3];3];2]=[g.map(|i|old[1][i]),g.map(|i|new[1][i])];let mut axes=vec![];for q in a.into_iter().chain(b){axes.push(cross(sub(q[1],q[0]),sub(q[2],q[0])));}for qa in a{for qb in b{for (u,v)in [(0,1),(1,2),(2,0)]{for (r,s)in [(0,1),(1,2),(2,0)]{axes.push(cross(sub(qa[v],qa[u]),sub(qb[s],qb[r])));}}}}
 let mut separated=false;for axis in axes{if axis.iter().any(|x|!x.is_finite())||axis.iter().all(|x|*x==0.){continue;}for sign in [1.,-1.]{let axis=axis.map(|x|x*sign);let plausible=(0..2).all(|k|a[k].iter().all(|p|b[k].iter().all(|q|(0..3).map(|i|axis[i]*(q[i]-p[i])).sum::<f64>()>0.)));if plausible&&super::boundary_path::exact_fixed_axis_separation(a[0],b[0],axis).map_err(fail)?&&super::boundary_path::exact_fixed_axis_separation(a[1],b[1],axis).map_err(fail)?{separated=true;break;}}if separated{break;}}
 if separated{report.exact_axis_separated+=1;continue;}let ea=[[f[0],f[1]],[f[1],f[2]],[f[2],f[0]]];let eb=[[g[0],g[1]],[g[1],g[2]],[g[2],g[0]]];report.unresolved.push(SweptCandidate{facets:[i,j],vertex_face:std::array::from_fn(|k|if k<3{([0,f[k]],j)}else{([1,g[k-3]],i)}),edge_edge:std::array::from_fn(|k|[ea[k/3],eb[k%3]])});}}
 Ok(report)
}
