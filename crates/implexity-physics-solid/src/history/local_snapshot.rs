// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::sync::Arc;
use rayon::prelude::*;
use implexity_core::error::{CaeError,CaeResult};
use crate::solid_history::SolidKernel;
use super::local_advance::{advance_local_substeps,LocalAdvanceOptions,LocalAdvanceRequest,LocalAdvanceResult};

#[derive(Clone,Debug)]
pub struct NativeLocalSnapshot {
    pub normalized_state:Vec<f64>,
    pub heat_density_J_m3:Vec<f64>,
    pub local_iterations:usize,
    pub maximum_local_residual:f64,
    pub maximum_local_condition:f64,
    kernel:Arc<SolidKernel>,
    local_maps:Vec<Vec<Option<usize>>>,
    design_maps:Vec<[usize;5]>,
    advances:Vec<LocalAdvanceResult>,
}

fn local_map(kernel:&SolidKernel,e:usize)->Vec<Option<usize>> {
    let mut map=vec![None;kernel.model.local_width()];
    for (i,node) in kernel.mesh.tets[e].iter().enumerate() {
        map[i]=usize::try_from(kernel.tmap[*node]).ok();
        for a in 0..3 {map[4+3*i+a]=usize::try_from(kernel.umap[3*node+a]).ok();}
    }
    let start=kernel.n_t()+kernel.n_u()+e*kernel.internal_size;
    for i in 0..kernel.internal_size {map[16+i]=Some(start+i);}
    map
}
impl NativeLocalSnapshot {
    pub fn apply(&self,driving:&[f64],previous:&[f64],design:&[f64])->CaeResult<(Vec<f64>,Vec<f64>)> {
        let k=&self.kernel;let w=k.model.local_width();let n=k.internal_size;
        if driving.len()!=k.state_size || previous.len()!=k.state_size || design.len()!=2*k.nc+3 || driving.iter().chain(previous).chain(design).any(|v|!v.is_finite()) {return Err(CaeError::contract("native local snapshot tangent shape or values invalid"));}
        let mut state=driving.to_vec();let mut heat=vec![0.;k.ne];
        for e in 0..k.ne {
            let mut input=vec![0.;2*w+5];
            for i in 0..w {if let Some(j)=self.local_maps[e][i] {input[i]=driving[j];input[w+i]=previous[j];}}
            for i in 0..5 {input[2*w+i]=design[self.design_maps[e][i]];}
            let (h,q)=self.advances[e].apply(&input)?;
            let start=k.n_t()+k.n_u()+e*n;state[start..start+n].copy_from_slice(&h);heat[e]=q;
        }
        Ok((state,heat))
    }
    pub fn apply_transpose(&self,state:&[f64],heat:&[f64])->CaeResult<(Vec<f64>,Vec<f64>,Vec<f64>)> {
        let k=&self.kernel;let w=k.model.local_width();let n=k.internal_size;
        if state.len()!=k.state_size || heat.len()!=k.ne || state.iter().chain(heat).any(|v|!v.is_finite()) {return Err(CaeError::contract("native local snapshot transpose shape or values invalid"));}
        let mut driving=state.to_vec();driving[k.n_t()+k.n_u()..k.internal_stop].fill(0.);
        let mut previous=vec![0.;k.state_size];let mut design=vec![0.;2*k.nc+3];
        for e in 0..k.ne {
            let start=k.n_t()+k.n_u()+e*n;
            let input=self.advances[e].apply_transpose(&state[start..start+n],heat[e])?;
            for i in 0..w {if let Some(j)=self.local_maps[e][i] {driving[j]+=input[i];previous[j]+=input[w+i];}}
            for i in 0..5 {design[self.design_maps[e][i]]+=input[2*w+i];}
        }
        Ok((driving,previous,design))
    }
}

fn advance_native_snapshot_mode(kernel:&Arc<SolidKernel>,step:usize,driving:&[f64],previous:&[f64],design:&[f64],options:&LocalAdvanceOptions,substeps:usize,age_condition:Option<(bool,bool)>)->CaeResult<NativeLocalSnapshot> {
    if step==0 || step>=kernel.nt || driving.len()!=kernel.state_size || previous.len()!=kernel.state_size || design.len()!=2*kernel.nc+3 || driving.iter().chain(previous).chain(design).any(|v|!v.is_finite()) {return Err(CaeError::contract("native local snapshot step, shape or values invalid"));}
    let (current_data,previous_data)=kernel.local_data(step);
    let mut state=driving.to_vec();let mut heat=vec![0.;kernel.ne];let mut maps=Vec::with_capacity(kernel.ne);let mut design_maps=Vec::with_capacity(kernel.ne);let mut advances=Vec::with_capacity(kernel.ne);
    let mut iterations=0;let mut residual:f64=0.;let mut condition:f64=0.;
    let advance=|e:usize| {
        let current=kernel.element_local(step,e,driving,&current_data);let old=kernel.element_local(step-1,e,previous,&previous_data);let x=kernel.element_design(e,design);
        let mut local_options=options.clone();
        if let Some(capture)=options.rejected_state_capture.as_ref() {let mut scoped=(**capture).clone();scoped.provenance=serde_json::json!({"caller":scoped.provenance,"physical_step":step,"physical_time_s":kernel.times[step],"previous_time_s":kernel.times[step-1],"element":e,"element_nodes":kernel.mesh.tets[e]});local_options.rejected_state_capture=Some(Arc::new(scoped));}
        let result={let _quiet=implexity_solve::trace::suppress();{let request=LocalAdvanceRequest{model:&kernel.model,reference_gradients:&kernel.mesh.gradients[e],driving:&current,previous:&old,design:&x};match age_condition {Some((enabled,true))=>super::stress_relaxing_age::stress_relaxing_age_condition(&request,enabled),Some((enabled,false))=>super::aged_condition::frozen_reference_age_condition(&request,enabled),None=>advance_local_substeps(&request,&local_options,substeps)}}};
        result
    };
    let mut parallel:Option<std::vec::IntoIter<CaeResult<LocalAdvanceResult>>>=if age_condition == Some((true,true)) && options.rejected_state_capture.is_none() {(0..kernel.ne).into_par_iter().map(advance).collect::<Vec<_>>().into_iter().into()} else {None};
    for e in 0..kernel.ne {
        let result=match parallel.as_mut() {Some(results)=>results.next().expect("native local snapshot indexed result"),None=>advance(e)};
        let result=match result {Ok(v)=>v,Err(error)=>{implexity_solve::trace::point("split_history.local_failure",||implexity_solve::trace_fields!{"physical_step"=>step,"element"=>e,"reason"=>error.to_string()})?;return Err(error.context(&format!("native local history physical_step={}, time_s={}, previous_time_s={}, element={}",step,kernel.times[step],kernel.times[step-1],e)));}};
        let start=kernel.n_t()+kernel.n_u()+e*kernel.internal_size;
        state[start..start+kernel.internal_size].copy_from_slice(&result.normalized_history);heat[e]=result.integrated_heat_J_m3;
        iterations+=result.iterations;residual=residual.max(result.residual_norm);condition=condition.max(result.local_condition);
        maps.push(local_map(kernel,e));let o=kernel.mesh.owners[e];design_maps.push([o,kernel.nc,kernel.nc+1,kernel.nc+2,kernel.nc+3+o]);advances.push(result);
    }
    Ok(NativeLocalSnapshot{normalized_state:state,heat_density_J_m3:heat,local_iterations:iterations,maximum_local_residual:residual,maximum_local_condition:condition,kernel:Arc::clone(kernel),local_maps:maps,design_maps,advances})
}

impl NativeLocalSnapshot {
    pub fn heat_correction(&self,step:usize,current:&[f64],previous:&[f64],design:&[f64])->CaeResult<Vec<f64>> {
        self.validate_frame(step,current,previous,design)?;
        let k=&self.kernel;let (cd,pd)=k.local_data(step);let mut out=vec![0.;k.state_size];
        for e in 0..k.ne {
            let c=k.element_local(step,e,current,&cd);let p=k.element_local(step-1,e,previous,&pd);let x=k.element_design(e,design);
            let q=super::local_advance::endpoint_heat_replacement(&k.model,&k.mesh.gradients[e],&c,&p,&x,self.heat_density_J_m3[e])?;
            for i in 0..4 {if let Some(j)=self.local_maps[e][i] {out[j]+=q[i];}}
        }
        Ok(out)
    }
    fn validate_frame(&self,step:usize,current:&[f64],previous:&[f64],design:&[f64])->CaeResult<()> {
        let k=&self.kernel;
        if step==0 || step>=k.nt || current.len()!=k.state_size || previous.len()!=k.state_size || design.len()!=2*k.nc+3 || current.iter().chain(previous).chain(design).any(|v|!v.is_finite()) {return Err(CaeError::contract("native split heat frame invalid"));}
        Ok(())
    }
    pub fn heat_correction_jacobian(&self,kind:implexity_solve::local_assembly::Kind,step:usize,current:&[f64],previous:&[f64],design:&[f64])->CaeResult<implexity_linalg::sparse::CsrMatrix> {
        use implexity_ad::Dual;
        use implexity_solve::local_assembly::Kind;
        self.validate_frame(step,current,previous,design)?;
        let k=&self.kernel;let w=k.model.local_width();let (cd,pd)=k.local_data(step);let mut entries=Vec::new();
        for e in 0..k.ne {
            let c=k.element_local(step,e,current,&cd);let p=k.element_local(step-1,e,previous,&pd);let x=k.element_design(e,design);
            let count=if matches!(kind,Kind::Design) {5} else {w};
            for column in 0..count {
                let global=match kind {Kind::Current|Kind::Previous=>self.local_maps[e][column],Kind::Design=>Some(self.design_maps[e][column])};
                let Some(global)=global else {continue};
                let mut dc:Vec<Dual<1>>=c.iter().map(|v|Dual::constant(*v)).collect();let mut dp:Vec<Dual<1>>=p.iter().map(|v|Dual::constant(*v)).collect();let mut dx:Vec<Dual<1>>=x.iter().map(|v|Dual::constant(*v)).collect();
                match kind {Kind::Current=>dc[column]=Dual::variable(c[column],0),Kind::Previous=>dp[column]=Dual::variable(p[column],0),Kind::Design=>dx[column]=Dual::variable(x[column],0)}
                let correction=super::local_advance::endpoint_heat_replacement(&k.model,&k.mesh.gradients[e],&dc,&dp,&dx,Dual::constant(self.heat_density_J_m3[e]))?;
                for i in 0..4 {if let Some(row)=self.local_maps[e][i] {let value=correction[i].eps[0];if value!=0. {entries.push((row,global,value));}}}
            }
        }
        let columns=if matches!(kind,Kind::Design) {2*k.nc+3} else {k.state_size};
        let rows:Vec<usize>=entries.iter().map(|e|e.0).collect();let cols:Vec<usize>=entries.iter().map(|e|e.1).collect();let values:Vec<f64>=entries.iter().map(|e|e.2).collect();
        implexity_linalg::sparse::CsrMatrix::from_triplets(k.state_size,columns,&rows,&cols,&values).map_err(|e|CaeError::contract(e.to_string()))
    }
    pub fn local_output_adjoint(&self,step:usize,current:&[f64],previous:&[f64],design:&[f64],residual_adjoint:&[f64])->CaeResult<(Vec<f64>,Vec<f64>)> {
        self.validate_frame(step,current,previous,design)?;
        let k=&self.kernel;
        if residual_adjoint.len()!=k.state_size || residual_adjoint.iter().any(|v|!v.is_finite()) {return Err(CaeError::contract("native split residual adjoint invalid"));}
        let mut history_seed=vec![0.;k.state_size];
        for i in k.n_t()+k.n_u()..k.internal_stop {history_seed[i]=-residual_adjoint[i];}
        let mut heat_seed=vec![0.;k.ne];let (cd,_)=k.local_data(step);let scale=k.model.ks*k.model.ts*k.model.ls;
        for e in 0..k.ne {
            let c=k.element_local(step,e,current,&cd);let x=k.element_design(e,design);let volume=k.model.fields(&k.mesh.gradients[e],&c,&x).volume;
            let sum=(0..4).filter_map(|i|self.local_maps[e][i]).map(|j|residual_adjoint[j]).sum::<f64>();
            heat_seed[e]=-sum*volume/(4.*c[k.model.local_size()]*scale);
        }
        Ok((history_seed,heat_seed))
    }
}

pub fn advance_native_snapshot(kernel:&Arc<SolidKernel>,step:usize,driving:&[f64],previous:&[f64],design:&[f64],options:&LocalAdvanceOptions,substeps:usize)->CaeResult<NativeLocalSnapshot> {advance_native_snapshot_mode(kernel,step,driving,previous,design,options,substeps,None)}
pub fn native_age_condition_snapshot(kernel:&Arc<SolidKernel>,step:usize,driving:&[f64],previous:&[f64],design:&[f64],enabled:bool)->CaeResult<NativeLocalSnapshot> {advance_native_snapshot_mode(kernel,step,driving,previous,design,&LocalAdvanceOptions::default(),1,Some((enabled,false)))}

pub fn native_prescribed_relaxed_age_snapshot(kernel:&Arc<SolidKernel>,step:usize,driving:&[f64],previous:&[f64],design:&[f64],enabled:bool)->CaeResult<NativeLocalSnapshot> {advance_native_snapshot_mode(kernel,step,driving,previous,design,&LocalAdvanceOptions::default(),1,Some((enabled,true)))}
