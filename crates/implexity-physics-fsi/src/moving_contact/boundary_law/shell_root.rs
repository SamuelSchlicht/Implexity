// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use serde::{Serialize,Deserialize};
use super::{boundary_path::{self,PathPolicy,RootDomain},edge_path,swept_coverage::exhaustive_swept_candidates};
use std::collections::BTreeMap;
fn fail(s:&str)->CaeError{CaeError::contract(s)}
fn digest(s:&str)->bool{s.len()==64&&s.bytes().all(|b|b.is_ascii_hexdigit())}
#[derive(Clone,Debug,PartialEq,Eq,PartialOrd,Ord,Serialize,Deserialize)]
pub enum ShellRootFeature{VertexFace{vertex_body:usize,vertex:usize,face:usize},EdgeEdge{edges:[[usize;2];2]}}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub enum ShellPathOwner{NumericalPredictor,PhysicalLinearSegment}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct ShellRootCandidate{pub feature:ShellRootFeature,pub family_id:String,pub bracket:[f64;2],pub exact_fraction:Option<f64>,pub transverse:bool,pub strict_feature:bool}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct ShellFeatureIdentity{pub feature:ShellRootFeature,pub family_id:String}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub enum ShellRootStatus{NoEvent,Localized,Refused}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct ShellRootReport{pub identity:String,pub source_identity:String,pub design_identity:String,pub path_owner:ShellPathOwner,pub physical_interval:[f64;2],pub status:ShellRootStatus,pub refusal:Option<String>,pub examined_facet_pairs:usize,pub separated_facet_pairs:usize,pub unique_feature_candidates:usize,pub feature_aabb_separated:usize,pub feature_exact_axis_separated:usize,pub coplanarity_roots:usize,pub outside_feature_roots:usize,pub earliest_bracket:Option<[f64;2]>,pub exact_fraction:Option<f64>,pub candidate_features:Vec<ShellFeatureIdentity>,pub simultaneous:Vec<ShellRootCandidate>}
fn positions(f:&ShellRootFeature,points:[&[[f64;3]];2],facets:[&[[usize;3]];2])->[[f64;3];4]{match *f{ShellRootFeature::VertexFace{vertex_body,vertex,face}=>{let other=1-vertex_body;let tri=facets[other][face];[points[vertex_body][vertex],points[other][tri[0]],points[other][tri[1]],points[other][tri[2]]]},ShellRootFeature::EdgeEdge{edges}=>[points[0][edges[0][0]],points[0][edges[0][1]],points[1][edges[1][0]],points[1][edges[1][1]]]}}
fn aabb_separated(f:&ShellRootFeature,a:[[f64;3];4],b:[[f64;3];4])->bool{let parts:&[(&[usize],&[usize])]=match f{ShellRootFeature::VertexFace{..}=>&[(&[0],&[1,2,3])],ShellRootFeature::EdgeEdge{..}=>&[(&[0,1],&[2,3])]};parts.iter().any(|(x,y)|(0..3).any(|k|{let range=|ids:&[usize]|{let mut lo=f64::INFINITY;let mut hi=f64::NEG_INFINITY;for i in ids{lo=lo.min(a[*i][k]).min(b[*i][k]);hi=hi.max(a[*i][k]).max(b[*i][k]);}[lo,hi]};let p=range(x);let q=range(y);p[1]<q[0]||q[1]<p[0]}))}

fn fixed_axis_separated(f:&ShellRootFeature,a:[[f64;3];4],b:[[f64;3];4])->CaeResult<bool>{
 let sets=match f{ShellRootFeature::VertexFace{..}=>[[0,0,0],[1,2,3]],ShellRootFeature::EdgeEdge{..}=>[[0,1,1],[2,3,3]]};let sub=|x:[f64;3],y:[f64;3]|std::array::from_fn::<_,3,_>(|k|x[k]-y[k]);let cross=|x:[f64;3],y:[f64;3]|[x[1]*y[2]-x[2]*y[1],x[2]*y[0]-x[0]*y[2],x[0]*y[1]-x[1]*y[0]];let mut axes=vec![];
 for p in [a,b]{for ids in sets{for (i,j) in [(0,1),(1,2),(2,0)]{let u=sub(p[ids[j]],p[ids[i]]);for basis in [[1.,0.,0.],[0.,1.,0.],[0.,0.,1.]]{axes.push(cross(u,basis));}}}for x in sets[0]{for y in sets[1]{axes.push(sub(p[y],p[x]));}}}
 for axis in axes{if axis.iter().any(|v|!v.is_finite())||axis.iter().all(|v|*v==0.){continue;}for sign in [1.,-1.]{let n=axis.map(|v|v*sign);if boundary_path::exact_fixed_axis_separation(sets[0].map(|i|a[i]),sets[1].map(|i|a[i]),n).map_err(fail)?&&boundary_path::exact_fixed_axis_separation(sets[0].map(|i|b[i]),sets[1].map(|i|b[i]),n).map_err(fail)?{return Ok(true);}}}Ok(false)
}
pub fn validate_shell_root_policy(p:PathPolicy)->CaeResult<()>{if p.minimum_barycentric!=0.||p.minimum_signed_gap!=0.||!p.minimum_area_ratio.is_finite()||p.minimum_area_ratio<=0.||p.minimum_area_ratio>=1.||!p.time_resolution.is_finite()||p.time_resolution<=0.||p.time_resolution>=1.||p.maximum_intervals==0||p.maximum_depth==0||p.maximum_depth>60{return Err(fail("strict shell root numerical policy"));}Ok(())}
pub fn earliest_complete_shell_root(old:[&[[f64;3]];2],new:[&[[f64;3]];2],facets:[&[[usize;3]];2],maximum_pairs:usize,policy:PathPolicy,source_identity:&str,design_identity:&str,physical_interval:[f64;2],path_owner:ShellPathOwner)->CaeResult<ShellRootReport>{
 if !digest(source_identity)||!digest(design_identity)||physical_interval.iter().any(|t|!t.is_finite())||physical_interval[1]<=physical_interval[0]||!(physical_interval[1]-physical_interval[0]).is_finite(){return Err(fail("shell root source/design/clock contract"));}
 validate_shell_root_policy(policy)?;let broad=exhaustive_swept_candidates(old,new,facets,maximum_pairs)?;let mut features=BTreeMap::new();
 for pair in &broad.unresolved{for ([body,vertex],face) in pair.vertex_face{features.insert(ShellRootFeature::VertexFace{vertex_body:body,vertex,face},());}for mut edges in pair.edge_edge{for e in &mut edges{e.sort_unstable();}features.insert(ShellRootFeature::EdgeEdge{edges},());}}
 let mut report=ShellRootReport{identity:String::new(),source_identity:source_identity.into(),design_identity:design_identity.into(),path_owner,physical_interval,status:ShellRootStatus::NoEvent,refusal:None,examined_facet_pairs:broad.examined_pairs,separated_facet_pairs:broad.aabb_separated+broad.exact_axis_separated,unique_feature_candidates:features.len(),feature_aabb_separated:0,feature_exact_axis_separated:0,coplanarity_roots:0,outside_feature_roots:0,earliest_bracket:None,exact_fraction:None,candidate_features:features.keys().map(|f|ShellFeatureIdentity{feature:f.clone(),family_id:implexity_core::json::canonical_sha256(&serde_json::json!({"source":source_identity,"feature":f}))}).collect(),simultaneous:vec![]};let mut events=vec![];
 for (feature,()) in features{let a=positions(&feature,old,facets);let b=positions(&feature,new,facets);if aabb_separated(&feature,a,b){report.feature_aabb_separated+=1;continue;}if fixed_axis_separated(&feature,a,b)?{report.feature_exact_axis_separated+=1;continue;}let roots=boundary_path::isolate_coplanarity_roots(a,b,policy).map_err(fail)?;report.coplanarity_roots+=roots.len();for root in roots{let domain=match feature{ShellRootFeature::VertexFace{..}=>boundary_path::vertex_face_root_domain(a,b,root.interval,policy.minimum_area_ratio).map_err(fail)?,ShellRootFeature::EdgeEdge{..}=>edge_path::edge_root_domain(a,b,root.interval,policy.minimum_area_ratio)?};if domain==RootDomain::Outside{report.outside_feature_roots+=1;continue;}let family_id=implexity_core::json::canonical_sha256(&serde_json::json!({"source":source_identity,"feature":feature}));events.push(ShellRootCandidate{feature:feature.clone(),family_id,bracket:root.interval,exact_fraction:root.exact_fraction,transverse:root.transverse,strict_feature:domain==RootDomain::Interior});}}
 events.sort_by(|a,b|a.bracket[0].total_cmp(&b.bracket[0]).then(a.bracket[1].total_cmp(&b.bracket[1])).then(a.feature.cmp(&b.feature)));
 if let Some(first)=events.first(){let end=first.bracket[1];report.earliest_bracket=Some(first.bracket);report.simultaneous=events.iter().take_while(|r|r.bracket[0]<=end).cloned().collect();let exact=first.exact_fraction;let tied=report.simultaneous.iter().all(|r|r.exact_fraction==exact&&r.bracket==first.bracket);report.exact_fraction=if tied{exact}else{None};let refusal=if first.bracket[0]==0.{Some("initial or persistently coplanar feature needs qualified existing-contact/degeneracy owner")}else if report.simultaneous.iter().any(|r|!r.transverse||!r.strict_feature){Some("earliest root has grazing, parallel, perimeter or unresolved feature domain")}else if report.simultaneous.len()>1&&(!tied||exact.is_none()){Some("overlapping root brackets do not certify simultaneous root identities or ordering")}else{None};report.status=if refusal.is_some(){ShellRootStatus::Refused}else{ShellRootStatus::Localized};report.refusal=refusal.map(str::to_owned);}
 report.identity=implexity_core::json::canonical_sha256(&serde_json::json!({"endpoint_identity":broad.endpoint_identity,"policy":{"resolution":policy.time_resolution,"intervals":policy.maximum_intervals,"depth":policy.maximum_depth,"area_ratio":policy.minimum_area_ratio},"report":report}));Ok(report)
}

#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct NativeShellRootReport{pub identity:String,pub surface_identity:String,pub previous_state_identity:String,pub current_state_identity:String,pub multipliers:usize,pub root:ShellRootReport}
pub fn earliest_native_shell_root(surface:&super::plane_manifold::CompleteSurfaceTrace,multipliers:usize,previous:&[f64],current:&[f64],design:&[f64],maximum_pairs:usize,policy:PathPolicy,physical_interval:[f64;2],path_owner:ShellPathOwner)->CaeResult<NativeShellRootReport>{
 if previous.iter().chain(current).chain(design).any(|v|!v.is_finite()){return Err(fail("native shell root nonfinite state/design"));}
 let old=surface.positions(previous,multipliers,design)?;let new=surface.positions(current,multipliers,design)?;
 let hash=|v:&[f64]|implexity_core::json::canonical_sha256(&serde_json::json!(v.iter().map(|x|x.to_bits()).collect::<Vec<_>>()));
 let root=earliest_complete_shell_root([&old[0],&old[1]],[&new[0],&new[1]],surface.facets(),maximum_pairs,policy,surface.identity(),&hash(design),physical_interval,path_owner)?;
 let mut out=NativeShellRootReport{identity:String::new(),surface_identity:surface.identity().into(),previous_state_identity:hash(previous),current_state_identity:hash(current),multipliers,root};out.identity=implexity_core::json::canonical_sha256(&serde_json::json!(out));Ok(out)
}
