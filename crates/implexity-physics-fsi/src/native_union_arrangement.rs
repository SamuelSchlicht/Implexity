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

use crate::gas_transient_kinetic::TransientGasScalar;
use crate::native_union_quadrature::{UnionNode, UnionSample, triangle};
use implexity_ad::Scalar;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
pub struct PhysicalSegment2<S: Scalar> {
    pub owner: Vec<usize>,
    pub endpoints: [UnionNode<S>; 2],
}

pub struct LabeledUnionSample<S: Scalar, L> {
    pub label: L,
    pub sample: UnionSample<S>,
    pub(crate) receipt: OutputSweepReceipt,
}

fn cross<S: Scalar>(a: [S; 2], b: [S; 2]) -> S { a[0] * b[1] - a[1] * b[0] }
fn difference<S: Scalar>(a: [S; 2], b: [S; 2]) -> [S; 2] { [a[0]-b[0],a[1]-b[1]] }
fn same<S: TransientGasScalar>(a: S, b: S) -> bool {
    match(a.geometry_first_order_directions(),b.geometry_first_order_directions()) {
        (Some(x),Some(y))=>a.value().to_bits()==b.value().to_bits()&&x.iter().map(|v|v.to_bits()).eq(y.iter().map(|v|v.to_bits())),
        _=>false,
    }
}
fn compatible_plane<S: TransientGasScalar>(a: [S;2],b: [S;2],p: [S;2]) -> Result<(),String> {
    let edge=difference(b,a);let delta=difference(p,a);let residual=cross(edge,delta);let gamma=64.*f64::EPSILON/(1.-64.*f64::EPSILON);
    let values=|x:[S;2]|x.map(|v|v.value().abs());let av=values(a);let bv=values(b);let pv=values(p);let ev=values(edge);let dv=values(delta);
    let constituent=|e:[f64;2],d:[f64;2],aa:[f64;2],bb:[f64;2],pp:[f64;2]|e[0]*(pp[1]+aa[1])+e[1]*(pp[0]+aa[0])+d[1]*(bb[0]+aa[0])+d[0]*(bb[1]+aa[1]);
    if !residual.value().is_finite() || residual.value().abs()>gamma*constituent(ev,dv,av,bv,pv) {return Err("physical union intersection does not satisfy its supporting segment".into());}
    let directions=residual.geometry_first_order_directions().ok_or("physical union intersection directions unavailable")?;
    let jets=|x:[S;2]|->Result<[Vec<f64>;2],String>{Ok([x[0].geometry_first_order_directions().ok_or("physical union constituent directions unavailable")?,x[1].geometry_first_order_directions().ok_or("physical union constituent directions unavailable")?])};
    let aj=jets(a)?;let bj=jets(b)?;let pj=jets(p)?;let ej=jets(edge)?;let dj=jets(delta)?;
    if [&aj,&bj,&pj,&ej,&dj].iter().any(|j|j.iter().any(|v|v.len()!=directions.len())) {return Err("physical union supporting direction shape".into());}
    for (k,d) in directions.iter().enumerate(){let at=|j:&[Vec<f64>;2]|[j[0][k].abs(),j[1][k].abs()];let budget=constituent(at(&ej),dv,av,bv,pv)+constituent(ev,at(&dj),at(&aj),at(&bj),at(&pj));if !d.is_finite()||d.abs()>gamma*budget{return Err("physical union segment intersection has incompatible motion".into());}}
    Ok(())
}

pub(crate) fn strictly_separated<S: TransientGasScalar>(a:[[S;2];2],b:[[S;2];2])->bool {
    (0..2).any(|k| {
        let aa=a.map(|p|p[k].value());let bb=b.map(|p|p[k].value());
        aa[0].max(aa[1])<bb[0].min(bb[1]) || bb[0].max(bb[1])<aa[0].min(aa[1])
    })
}

pub fn positive_labeled_union<S: TransientGasScalar,L:Clone>(
    input: &[PhysicalSegment2<S>], refinement: usize, frame: [f64;2],
    classify: impl Fn([f64;2])->Result<Option<L>,String>,
) -> Result<Vec<LabeledUnionSample<S,L>>,String> {
    positive_labeled_union_with_source_incidence(input,&[],refinement,frame,classify)
}
pub(crate) fn positive_labeled_union_with_source_incidence<S:TransientGasScalar,L:Clone>(input:&[PhysicalSegment2<S>],declared:&[crate::native_physical_plane_preparation::SourceEndpointIncidence<S>],refinement:usize,frame:[f64;2],classify:impl Fn([f64;2])->Result<Option<L>,String>)->Result<Vec<LabeledUnionSample<S,L>>,String>{positive_labeled_union_with_source_incidence_and_sweep(input,declared,refinement,frame,|p,_|classify(p))}
pub(crate) fn positive_labeled_union_with_source_incidence_and_sweep<S:TransientGasScalar,L:Clone>(input:&[PhysicalSegment2<S>],declared:&[crate::native_physical_plane_preparation::SourceEndpointIncidence<S>],refinement:usize,frame:[f64;2],classify:impl Fn([f64;2],&SweepPointReceipt)->Result<Option<L>,String>)->Result<Vec<LabeledUnionSample<S,L>>,String>{
    if input.is_empty() || refinement>8 {return Err("empty physical union arrangement".into());}
    let [c,s]=frame;let determinant=c*c+s*s;
    if !determinant.is_finite() || determinant<=0. {return Err("invalid physical union arrangement frame".into());}
    let invocation=OutputSweepInvocation(std::sync::Arc::new(()));
    let mut nodes=Vec::<UnionNode<S>>::new();let mut identities=BTreeMap::<Vec<usize>,usize>::new();let mut segments=Vec::<([usize;2],Vec<usize>)>::new();let mut segment_keys=BTreeSet::new();
    let insert=|node:UnionNode<S>,nodes:&mut Vec<UnionNode<S>>,identities:&mut BTreeMap<Vec<usize>,usize>|->Result<usize,String>{
        if let Some(&index)=identities.get(&node.feature){if (0..2).any(|k|!same(nodes[index].point[k],node.point[k])){return Err("physical union semantic node has incompatible coordinates/motions".into());}Ok(index)}else{let index=nodes.len();identities.insert(node.feature.clone(),index);nodes.push(node);Ok(index)}
    };
    let mut raw_points=BTreeMap::<usize,[S;2]>::new();
    for segment in input {
        if segment.owner.is_empty(){return Err("physical union segment has no authentic owner".into());}
        let mut endpoints=[0;2];for(i,node)in segment.endpoints.iter().enumerate(){if node.feature.is_empty(){return Err("physical union endpoint has no authentic feature".into());}if node.point.iter().any(|v|!v.finite_first_order()||v.geometry_first_order_directions().is_none()){return Err("physical union endpoint directions are unavailable or non-finite".into());}let [x,y]=node.point;endpoints[i]=insert(UnionNode{feature:node.feature.clone(),point:[x*c+y*s,y*c-x*s]},&mut nodes,&mut identities)?;raw_points.insert(endpoints[i],node.point);}
        if endpoints[0]==endpoints[1]{return Err("physical union segment has coincident semantic endpoints".into());}
        let key=(segment.owner.clone(),endpoints[0].min(endpoints[1]),endpoints[0].max(endpoints[1]));
        if segment_keys.insert(key){segments.push((endpoints,segment.owner.clone()));}
    }
    let mut incidences=BTreeMap::<(usize,usize),()>::new();for (i,(ends,_)) in segments.iter().enumerate(){for &node in ends{incidences.insert((i,node),());}}
    let mut source_incidence=BTreeMap::<(usize,usize),usize>::new();
    for token in declared{let node=*identities.get(token.feature()).ok_or("source endpoint incidence node is absent")?;if(0..2).any(|k|!same(raw_points[&node][k],token.point()[k])){return Err("source endpoint incidence full point differs".into());}let mut found=false;for (i,(ends,owner)) in segments.iter().enumerate(){if owner.as_slice()!=token.source_owner()||!ends.contains(&node){continue;}for(j,(other,target))in segments.iter().enumerate(){if i==j||target.as_slice()!=token.target_owner(){continue;}let[a,b]=other.map(|n|raw_points[&n]);compatible_plane(a,b,token.point())?;let axis=usize::from((b[1]-a[1]).value().abs()>(b[0]-a[0]).value().abs());let x=token.point()[axis].value();if x<a[axis].value().min(b[axis].value())||x>a[axis].value().max(b[axis].value()){continue;}incidences.insert((j,node),());let key=(i.min(j),i.max(j));if let Some(old)=source_incidence.insert(key,node){if old!=node&&(0..2).any(|k|!same(nodes[old].point[k],nodes[node].point[k])){return Err("source endpoint incidence is ambiguous".into());}}found=true;}}if !found{return Err("source endpoint incidence owner differs".into());}}
    for i in 0..segments.len(){for j in i+1..segments.len(){
        let (first,first_owner)=&segments[i];let (second,second_owner)=&segments[j];
        let shared_endpoint=first.iter().any(|index|second.contains(index));
        if strictly_separated(first.map(|id|raw_points[&id]),second.map(|id|raw_points[&id])) {continue;}
        let [a,b]=first.map(|index|nodes[index].point);let [p,q]=second.map(|index|nodes[index].point);let r=difference(b,a);let t=difference(q,p);let denom=cross(r,t);
        if denom.value()==0. {
            if cross(r,difference(p,a)).value()==0. {
                let axis=usize::from(r[1].value().abs()>r[0].value().abs());
                let overlap=a[axis].value().max(b[axis].value()).min(p[axis].value().max(q[axis].value()))-a[axis].value().min(b[axis].value()).max(p[axis].value().min(q[axis].value()));
                if overlap>0.{return Err(format!("coincident physical union segments need an authenticated closed mask owner i={i} j={j} owner_i={first_owner:?} owner_j={second_owner:?} overlap={overlap:.17e} first_normalized={:?} second_normalized={:?} first_raw={:?} second_raw={:?}",[a,b].map(|point|point.map(|v|(v.value(),v.geometry_first_order_directions()))),[p,q].map(|point|point.map(|v|(v.value(),v.geometry_first_order_directions()))),first.map(|id|raw_points[&id].map(|v|(v.value(),v.geometry_first_order_directions()))),second.map(|id|raw_points[&id].map(|v|(v.value(),v.geometry_first_order_directions())))));}
            }
            continue;
        }
        if shared_endpoint {continue;}
        if source_incidence.contains_key(&(i,j)){continue;}
        let scale=(r[0].value().abs()+r[1].value().abs())*(t[0].value().abs()+t[1].value().abs());
        let mut authentic_endpoint:Option<usize>=None;
        for (ends,other) in [(first,second),(second,first)] {let [oa,ob]=other.map(|id|raw_points[&id]);let edge=difference(ob,oa);let axis=usize::from(edge[1].value().abs()>edge[0].value().abs());for &index in ends{let raw=raw_points[&index];if cross(edge,difference(raw,oa)).value()==0.&&edge[axis].value()!=0.{let fraction=(raw[axis]-oa[axis])/edge[axis];if (0. ..=1.).contains(&fraction.value()){compatible_plane(oa,ob,raw)?;if let Some(previous)=authentic_endpoint {if (0..2).any(|k|!same(nodes[previous].point[k],nodes[index].point[k])){return Err("ambiguous exact physical endpoint incidence".into());}}authentic_endpoint=Some(index);}}}}
        if let Some(index)=authentic_endpoint{incidences.insert((i,index),());incidences.insert((j,index),());continue;}
        let u=cross(difference(p,a),t)/denom;let v=cross(difference(p,a),r)/denom;
        if u.value()<0. || u.value()>1. || v.value()<0. || v.value()>1. {continue;}
        if denom.value().abs()<=1024.*f64::EPSILON*scale{return Err(format!("physical union segment crossing loses transverse rank i={i} j={j} owner_i={first_owner:?} owner_j={second_owner:?} features_i={:?} features_j={:?} raw_i={:?} raw_j={:?} normalized_i={:?} normalized_j={:?} denom={:?} scale={scale:e} u={:?} v={:?}",first.map(|id|nodes[id].feature.clone()),second.map(|id|nodes[id].feature.clone()),first.map(|id|raw_points[&id].map(|q|(q.value(),q.geometry_first_order_directions()))),second.map(|id|raw_points[&id].map(|q|(q.value(),q.geometry_first_order_directions()))),[a,b].map(|p|p.map(|q|(q.value(),q.geometry_first_order_directions()))),[p,q].map(|p|p.map(|q|(q.value(),q.geometry_first_order_directions()))),(denom.value(),denom.geometry_first_order_directions()),(u.value(),u.geometry_first_order_directions()),(v.value(),v.geometry_first_order_directions())));}
        let candidate=[a[0]+r[0]*u,a[1]+r[1]*u];compatible_plane(a,b,candidate)?;compatible_plane(p,q,candidate)?;
        let endpoint=if u.value()==0.{Some(first[0])}else if u.value()==1.{Some(first[1])}else if v.value()==0.{Some(second[0])}else if v.value()==1.{Some(second[1])}else{None};
        if let Some(index)=endpoint {compatible_plane(a,b,nodes[index].point)?;compatible_plane(p,q,nodes[index].point)?;incidences.insert((i,index),());incidences.insert((j,index),());}
        else {
            let (left,right)=if first_owner<=second_owner{(first_owner,second_owner)}else{(second_owner,first_owner)};
            let mut feature=vec![usize::MAX,left.len()];feature.extend(left);feature.push(right.len());feature.extend(right);
            for &index in first.iter().chain(second){feature.push(nodes[index].feature.len());feature.extend(&nodes[index].feature);}
            let node=insert(UnionNode{feature,point:candidate},&mut nodes,&mut identities)?;incidences.insert((i,node),());incidences.insert((j,node),());
        }
    }}
    let mut columns:Vec<_>=(0..nodes.len()).collect();columns.sort_by(|&a,&b|nodes[a].point[0].value().total_cmp(&nodes[b].point[0].value()).then(nodes[a].feature.cmp(&nodes[b].feature)));
    let mut unique=Vec::<usize>::new();for index in columns{if let Some(&previous)=unique.last(){if nodes[index].point[0].value()==nodes[previous].point[0].value(){if !same(nodes[index].point[0],nodes[previous].point[0]){return Err("physical arrangement sweep columns need a different fixed frame".into());}continue;}}unique.push(index);}
    let mut output=Vec::new();
    for pair in unique.windows(2){
        let left=nodes[pair[0]].point[0];let right=nodes[pair[1]].point[0];let middle=(left.value()+right.value())*0.5;let mut crossings=Vec::new();
        for (index,(ends,_)) in segments.iter().enumerate(){let [a,b]=ends.map(|index|nodes[index].point);if a[0].value().min(b[0].value())<middle && middle<a[0].value().max(b[0].value()){let fraction=(middle-a[0].value())/(b[0].value()-a[0].value());crossings.push((a[1].value()+(b[1].value()-a[1].value())*fraction,index));}}
        crossings.sort_by(|a,b|a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        if crossings.windows(2).any(|pair|pair[0].0==pair[1].0){return Err("physical arrangement has an unresolved coincident mask boundary".into());}
        let at=|index:usize,x:S|{for (&(segment,node),()) in &incidences{if segment==index &&same(nodes[node].point[0],x){return nodes[node].point;}}let [a,b]=segments[index].0.map(|index|nodes[index].point);if same(x,a[0]){return a;}if same(x,b[0]){return b;}let fraction=(x-a[0])/(b[0]-a[0]);[x,a[1]+(b[1]-a[1])*fraction]};
        for crossings in crossings.windows(2){let middle_y=(crossings[0].0+crossings[1].0)*0.5;let point=[(middle*c-middle_y*s)/determinant,(middle*s+middle_y*c)/determinant];
            let receipt=SweepPointReceipt{exact:ExactSweepPoint::from_actual_inverse(frame,[middle,middle_y],point)};if let Some(label)=classify(point,&receipt).map_err(|e|{let a=at(crossings[0].1,left);let b=at(crossings[0].1,right);let d=at(crossings[1].1,left);let end=at(crossings[1].1,right);let uv=|q:[S;2]|[(q[0].value()*c-q[1].value()*s)/determinant,(q[0].value()*s+q[1].value()*c)/determinant];format!("{e}; arrangement_source_slab frame=({c:?},{s:?}) determinant={determinant:?} left={:?} right={:?} middle={middle:?} middle_y={middle_y:?} lower_crossing={:?} upper_crossing={:?} lower_owner={:?} upper_owner={:?} lower_source_endpoints={:?} upper_source_endpoints={:?} retained_slab_uv={:?} retained_slab_frame={:?}",(left.value(),left.geometry_first_order_directions()),(right.value(),right.geometry_first_order_directions()),crossings[0],crossings[1],segments[crossings[0].1].1,segments[crossings[1].1].1,segments[crossings[0].1].0.map(|i|(nodes[i].feature.clone(),raw_points.get(&i).map(|q|q.map(|v|(v.value(),v.geometry_first_order_directions()))),nodes[i].point.map(|v|(v.value(),v.geometry_first_order_directions())))),segments[crossings[1].1].0.map(|i|(nodes[i].feature.clone(),raw_points.get(&i).map(|q|q.map(|v|(v.value(),v.geometry_first_order_directions()))),nodes[i].point.map(|v|(v.value(),v.geometry_first_order_directions())))),[a,b,end,d].map(uv),[a,b,end,d].map(|p|p.map(|q|(q.value(),q.geometry_first_order_directions()))))})?{let a=at(crossings[0].1,left);let b=at(crossings[0].1,right);let d=at(crossings[1].1,left);let e=at(crossings[1].1,right);let mut samples=Vec::new();let measure_same=|a:S,b:S|match(a.geometry_first_order_directions(),b.geometry_first_order_directions()){(Some(x),Some(y))=>a.value()==b.value()&&x==y,_=>false};triangle([a,b,e],refinement,&measure_same,&mut samples)?;triangle([a,e,d],refinement,&measure_same,&mut samples)?;for sample in samples{let point=[(sample.point[0]*c-sample.point[1]*s)/determinant,(sample.point[0]*s+sample.point[1]*c)/determinant];let receipt=OutputSweepReceipt{invocation:invocation.clone(),uv_bits:output_uv_bits(point),sweep:SweepPointReceipt{exact:ExactSweepPoint::from_actual_inverse(frame,sample.point.map(|v|v.value()),point.map(|v|v.value()))}};output.push(LabeledUnionSample{label:label.clone(),sample:UnionSample{point,weight:sample.weight/determinant},receipt});}}
        }
    }
    Ok(output)
}

pub(crate) struct SweepPointReceipt{exact:Result<ExactSweepPoint,&'static str>}
impl SweepPointReceipt {
 pub(crate) fn validate_uv(&self,uv:[f64;2])->Result<(),&'static str>{if self.exact.as_ref().map_err(|e|*e)?.emitted().map(f64::to_bits)!=uv.map(f64::to_bits){return Err("source sweep emitted UV differs");}Ok(())}
 pub(crate) fn barycentric_sign(&self,corner:usize)->Result<i8,&'static str>{self.exact.as_ref().map_err(|e|*e)?.barycentric_sign(corner)}
 pub(crate) fn affine_sign(&self,corners:[f64;3],coordinate:f64)->Result<i8,&'static str>{self.exact.as_ref().map_err(|e|*e)?.affine_sign(corners,coordinate)}
 pub(crate) fn exact_framed_affine_plane_sign(&self,normal:[f64;3],corners:[[f64;3];3],corner_anchor:[f64;3],origin:[f64;3],origin_anchor:[f64;3],offset:f64)->Result<i8,&'static str>{self.exact.as_ref().map_err(|e|*e)?.exact_framed_affine_plane_sign(normal,corners,corner_anchor,origin,origin_anchor,offset)}
}
include!("native_union_arrangement/source_sweep_exact.rs");

#[derive(Clone,Debug)]
pub(crate) struct OutputSweepInvocation(std::sync::Arc<()>);
impl PartialEq for OutputSweepInvocation{fn eq(&self,other:&Self)->bool{std::sync::Arc::ptr_eq(&self.0,&other.0)}}
impl Eq for OutputSweepInvocation{}
pub(crate) struct OutputSweepReceipt{invocation:OutputSweepInvocation,uv_bits:Result<Vec<u64>,&'static str>,sweep:SweepPointReceipt}
fn output_uv_bits<S:TransientGasScalar>(uv:[S;2])->Result<Vec<u64>,&'static str>{let mut bits=Vec::new();for v in uv{if !v.finite_first_order(){return Err("source output UV is nonfinite");}bits.push(v.value().to_bits());let d=v.geometry_first_order_directions().ok_or("source output UV directions unavailable")?;bits.push(d.len()as u64);for x in d{if !x.is_finite(){return Err("source output UV direction is nonfinite");}bits.push(x.to_bits());}}Ok(bits)}
impl OutputSweepReceipt{pub(crate) fn invocation(&self)->OutputSweepInvocation{self.invocation.clone()}pub(crate) fn validate_invocation(&self,invocation:&OutputSweepInvocation)->Result<(),&'static str>{if &self.invocation!=invocation{return Err("source output sweep invocation differs");}Ok(())}pub(crate) fn validate<S:TransientGasScalar>(&self,uv:[S;2])->Result<&SweepPointReceipt,&'static str>{if self.uv_bits.as_ref().map_err(|e|*e)?!=&output_uv_bits(uv)?{return Err("source output UV full channels differ");}self.sweep.validate_uv(uv.map(|v|v.value()))?;Ok(&self.sweep)}}


