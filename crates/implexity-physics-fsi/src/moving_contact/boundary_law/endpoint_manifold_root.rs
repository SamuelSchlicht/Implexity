// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use serde::{Serialize,Deserialize};
use super::{boundary_path::{exact_projection_sign,oriented_supporting_cone_is_unique,PathPolicy},plane_manifold::CompleteSurfaceTrace,shell_root::{earliest_native_shell_root,NativeShellRootReport,ShellPathOwner}};
use std::collections::BTreeSet;
fn fail(s:&str)->CaeError{CaeError::contract(s)}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct EndpointVertexFamily{pub vertices:[usize;2],pub family_id:String,pub position_m:[f64;3],pub incident_facets:[Vec<usize>;2],pub native_star_rows:usize,pub cone_identity:String}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct EndpointManifoldRoot{pub identity:String,pub primitive_coverage:NativeShellRootReport,pub supporting_axis:[f64;3],pub anchor:[usize;2],pub positive_body:usize,pub fraction:f64,pub absolute_clock:f64,pub simultaneous:Vec<EndpointVertexFamily>,pub source_scope:String}
pub fn certify_native_endpoint_manifold_root(surface:&CompleteSurfaceTrace,multipliers:usize,previous:&[f64],current:&[f64],design:&[f64],maximum_pairs:usize,policy:PathPolicy,physical_interval:[f64;2],path_owner:ShellPathOwner,axis:[f64;3],anchor:[usize;2],positive_body:usize)->CaeResult<EndpointManifoldRoot>{
 super::shell_root::validate_shell_root_policy(policy)?;if anchor[0]>1||positive_body>1||anchor[0]==positive_body||axis.iter().any(|v|!v.is_finite())||axis.iter().all(|v|*v==0.){return Err(fail("endpoint supporting plane native selection"));}
 let old=surface.positions(previous,multipliers,design)?;let new=surface.positions(current,multipliers,design)?;let aold=*old[anchor[0]].get(anchor[1]).ok_or_else(||fail("endpoint native plane anchor"))?;let anew=new[anchor[0]][anchor[1]];let facets=surface.facets();let mut plane=[vec![],vec![]];
 for body in 0..2{for i in 0..old[body].len(){let s=if body==positive_body{1}else{-1};let o=exact_projection_sign(old[body][i],aold,axis).map_err(fail)?*s;let n=exact_projection_sign(new[body][i],anew,axis).map_err(fail)?*s;if o<0||n<0||(body==positive_body&&o==0){return Err(fail("complete affine shells lack strict pre-endpoint supporting separation"));}if n==0{plane[body].push(i);}}}
 if plane.iter().any(Vec::is_empty){return Err(fail("endpoint supporting surfaces do not meet"));}
 let mut pairs=vec![];let mut owned=[BTreeSet::new(),BTreeSet::new()];
 for &i in &plane[0]{for &j in &plane[1]{if new[0][i]==new[1][j]{if !owned[0].insert(i)||!owned[1].insert(j){return Err(fail("endpoint manifold has duplicated native vertex location"));}pairs.push([i,j]);}}}
 if owned[0].len()!=plane[0].len()||owned[1].len()!=plane[1].len(){return Err(fail("endpoint manifold has unmatched supporting vertices; general overlap owner required"));}
 let mut simultaneous=vec![];
 for pair in pairs{let mut rows=vec![];let mut incident=[vec![],vec![]];for body in 0..2{let mut star=BTreeSet::new();for (f,tri)in facets[body].iter().enumerate(){if tri.contains(&pair[body]){incident[body].push(f);for &k in tri{star.insert(k);}}}if incident[body].is_empty(){return Err(fail("endpoint vertex lacks complete incident native star"));}let sign=if body==positive_body{1.}else{-1.};for k in star{rows.push(std::array::from_fn(|c|sign*(new[body][k][c]-new[body][pair[body]][c])));}}
 if !oriented_supporting_cone_is_unique(&rows,axis).map_err(fail)?{return Err(fail("endpoint manifold supporting cone not unique; KKT owner required"));}
 let family_id=implexity_core::json::canonical_sha256(&serde_json::json!({"source":surface.identity(),"native_vertex_pair":pair,"kind":"complete-shell matched vertex manifold"}));let cone_identity=implexity_core::json::canonical_sha256(&serde_json::json!({"source":surface.identity(),"pair":pair,"positions":new[0][pair[0]],"axis":axis,"rows":rows,"incident_facets":incident}));simultaneous.push(EndpointVertexFamily{vertices:pair,family_id,position_m:new[0][pair[0]],incident_facets:incident,native_star_rows:rows.len(),cone_identity});
 }
 let primitive_coverage=earliest_native_shell_root(surface,multipliers,previous,current,design,maximum_pairs,policy,physical_interval,path_owner)?;
 let mut report=EndpointManifoldRoot{identity:String::new(),primitive_coverage,supporting_axis:axis,anchor,positive_body,fraction:1.,absolute_clock:physical_interval[1],simultaneous,source_scope:"exact fixed-axis whole-affine-shell pre-endpoint separation and complete matched supporting-vertex cones; no distributed force uniqueness or ordinary event gradient".into()};report.identity=implexity_core::json::canonical_sha256(&serde_json::json!(report));Ok(report)
}

#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct SupportingProjectionRoot{pub vertex:usize,pub bracket:[f64;2],pub exact_fraction:Option<f64>}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub enum SupportingLocalizationStatus{NoEvent,Localized,Refused}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct SupportingManifoldLocalization{pub identity:String,pub surface_identity:String,pub previous_state_identity:String,pub predicted_state_identity:String,pub design_identity:String,pub path_owner:ShellPathOwner,pub physical_interval:[f64;2],pub status:SupportingLocalizationStatus,pub refusal:Option<String>,pub supporting_roots:Vec<SupportingProjectionRoot>,pub earliest_bracket:Option<[f64;2]>,pub exact_fraction:Option<f64>,pub evaluated_clock:Option<f64>,pub absolute_clock_is_exact:bool,pub localized_state:Option<Vec<f64>>,pub manifold:Option<EndpointManifoldRoot>}
pub fn localize_native_supporting_manifold_root(surface:&CompleteSurfaceTrace,multipliers:usize,previous:&[f64],predicted:&[f64],design:&[f64],maximum_pairs:usize,policy:PathPolicy,physical_interval:[f64;2],path_owner:ShellPathOwner,axis:[f64;3],anchor:[usize;2],positive_body:usize)->CaeResult<SupportingManifoldLocalization>{
 super::shell_root::validate_shell_root_policy(policy)?;if anchor[0]>1||positive_body>1||anchor[0]==positive_body||physical_interval.iter().any(|t|!t.is_finite())||physical_interval[1]<=physical_interval[0]||!(physical_interval[1]-physical_interval[0]).is_finite(){return Err(fail("supporting localization native selection/clock"));}
 let old=surface.positions(previous,multipliers,design)?;let new=surface.positions(predicted,multipliers,design)?;let aold=*old[anchor[0]].get(anchor[1]).ok_or_else(||fail("supporting localization anchor"))?;let anew=new[anchor[0]][anchor[1]];
 for i in 0..old[anchor[0]].len(){if exact_projection_sign(old[anchor[0]][i],aold,axis).map_err(fail)?>0||exact_projection_sign(new[anchor[0]][i],anew,axis).map_err(fail)?>0{return Err(fail("supporting localization negative body leaves fixed-axis halfspace"));}}
 let mut roots=vec![];for i in 0..old[positive_body].len(){if let Some(r)=super::boundary_path::exact_affine_projection_root(old[positive_body][i],new[positive_body][i],aold,anew,axis,policy).map_err(fail)?{roots.push(SupportingProjectionRoot{vertex:i,bracket:r.interval,exact_fraction:r.exact_fraction});}}
 roots.sort_by(|a,b|a.bracket[0].total_cmp(&b.bracket[0]).then(a.bracket[1].total_cmp(&b.bracket[1])).then(a.vertex.cmp(&b.vertex)));
 let hash=|v:&[f64]|implexity_core::json::canonical_sha256(&serde_json::json!(v.iter().map(|x|x.to_bits()).collect::<Vec<_>>()));
 let mut out=SupportingManifoldLocalization{identity:String::new(),surface_identity:surface.identity().into(),previous_state_identity:hash(previous),predicted_state_identity:hash(predicted),design_identity:hash(design),path_owner,physical_interval,status:SupportingLocalizationStatus::NoEvent,refusal:None,supporting_roots:roots,earliest_bracket:None,exact_fraction:None,evaluated_clock:None,absolute_clock_is_exact:false,localized_state:None,manifold:None};
 if let Some(first)=out.supporting_roots.first(){out.earliest_bracket=Some(first.bracket);let tied=out.supporting_roots.iter().take_while(|r|r.bracket[0]<=first.bracket[1]).all(|r|r.exact_fraction==first.exact_fraction&&r.bracket==first.bracket);if !tied||first.exact_fraction.is_none(){out.status=SupportingLocalizationStatus::Refused;out.refusal=Some("supporting root ordering/fraction requires exact localization; bracket midpoint not admitted".into());}else{let fraction=first.exact_fraction.unwrap();out.exact_fraction=Some(fraction);let clock=physical_interval[0]+fraction*(physical_interval[1]-physical_interval[0]);let state:Vec<_>=previous.iter().zip(predicted).map(|(a,b)|a+fraction*(b-a)).collect();if !clock.is_finite()||clock<=physical_interval[0]||clock>physical_interval[1]{return Err(fail("supporting localized binary64 clock domain"));}out.evaluated_clock=Some(clock);out.absolute_clock_is_exact=super::boundary_path::exact_affine_clock_is_represented(clock,physical_interval,fraction).map_err(fail)?;match certify_native_endpoint_manifold_root(surface,multipliers,previous,&state,design,maximum_pairs,policy,[physical_interval[0],clock],path_owner,axis,anchor,positive_body){Ok(manifold)=>{out.localized_state=Some(state);out.manifold=Some(manifold);out.status=SupportingLocalizationStatus::Localized;},Err(e)=>{out.status=SupportingLocalizationStatus::Refused;out.refusal=Some(format!("localized supporting endpoint refused: {e}"));}}}}
 out.identity=implexity_core::json::canonical_sha256(&serde_json::json!(out));Ok(out)
}
