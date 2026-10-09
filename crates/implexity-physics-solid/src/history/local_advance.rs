// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_ad::{Dual, Scalar};
use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::{DenseLu, DenseMatrix};
use crate::solid_elements::SolidModel;
use super::HistoryLaw;
use std::sync::Arc;
use implexity_solve::implicit_block::{BlockCallbacks,BlockOptions,ImplicitBlockSystem};
use implexity_solve::matrix::Jacobian;

#[derive(Clone, Debug)]
pub struct LocalAdvanceOptions {
    pub tolerance: f64,
    pub maximum_iterations: usize,
    pub condition_limit: f64,
    pub rejected_state_capture: Option<Arc<implexity_solve::rejected_state::RejectedStateCapture>>,
}
impl Default for LocalAdvanceOptions {
    fn default() -> Self { Self { tolerance: 1e-8, maximum_iterations: 60, condition_limit: 1e14, rejected_state_capture: None } }
}

pub struct LocalAdvanceRequest<'a> {
    pub model: &'a SolidModel,
    pub reference_gradients: &'a [[f64; 3]; 4],
    pub driving: &'a [f64],
    pub previous: &'a [f64],
    pub design: &'a [f64],
}

#[derive(Clone, Debug)]
pub struct LocalAdvanceResult {
    pub normalized_history: Vec<f64>,
    pub integrated_heat_J_m3: f64,
    pub residual_norm: f64,
    pub iterations: usize,
    pub local_condition: f64,
    pub input_width: usize,
    pub state_input_jacobian: DenseMatrix,
    pub heat_input_gradient: Vec<f64>,
}
impl LocalAdvanceResult {
    pub fn apply(&self, input: &[f64]) -> CaeResult<(Vec<f64>, f64)> {
        if input.len() != self.input_width { return Err(CaeError::contract("local history tangent input width mismatch")); }
        let h = (0..self.normalized_history.len()).map(|i| input.iter().enumerate().map(|(j,v)| self.state_input_jacobian.get(i,j) * v).sum()).collect();
        Ok((h, self.heat_input_gradient.iter().zip(input).map(|(a,b)| a*b).sum()))
    }
    pub fn apply_transpose(&self, state: &[f64], heat: f64) -> CaeResult<Vec<f64>> {
        if state.len() != self.normalized_history.len() { return Err(CaeError::contract("local history transpose input width mismatch")); }
        Ok((0..self.input_width).map(|j| state.iter().enumerate().map(|(i,v)| self.state_input_jacobian.get(i,j)*v).sum::<f64>()+heat*self.heat_input_gradient[j]).collect())
    }
}

fn evaluate<S: Scalar>(r: &LocalAdvanceRequest<'_>, h: &[S], input: &[S]) -> (Vec<S>, S) {
    let w = r.model.local_width();
    let mut current = input[..w].to_vec();
    current[16..16+h.len()].copy_from_slice(h);
    let previous = &input[w..2*w];
    let design = &input[2*w..];
    let mut residual = vec![S::zero();r.model.local_size()];
    r.model.residual(r.reference_gradients,&current,previous,design,&mut residual);
    let dt = current[r.model.local_size()];
    let forcing:Vec<f64>=current[r.model.local_size()+2..].iter().map(Scalar::value).collect();
    let (_,obs)=r.model.observables(r.reference_gradients,&current,previous,design,dt,&forcing);
    (residual[16..].to_vec(),obs.heat_increment)
}
fn norm(v: &[f64]) -> f64 { v.iter().map(|x|x*x).sum::<f64>().sqrt() }
fn linearize(r: &LocalAdvanceRequest<'_>, h: &[f64], input: &[f64], include_inputs:bool) -> (DenseMatrix,DenseMatrix,Vec<f64>,Vec<f64>) {
    let n=h.len();let m=input.len();
    let mut a=DenseMatrix::zeros(n,n);let mut b=DenseMatrix::zeros(n,m);let mut qh=vec![0.;n];let mut qi=vec![0.;m];
    for k in 0..n+if include_inputs {m} else {0} {
        let mut hd:Vec<Dual<1>>=h.iter().map(|v|Dual::constant(*v)).collect();
        let mut id:Vec<Dual<1>>=input.iter().map(|v|Dual::constant(*v)).collect();
        if k<n { hd[k]=Dual::variable(h[k],0); } else { id[k-n]=Dual::variable(input[k-n],0); }
        let (rd,qd)=evaluate(r,&hd,&id);
        for i in 0..n { if k<n { a.data[i*n+k]=rd[i].eps[0]; } else { b.data[i*m+k-n]=rd[i].eps[0]; } }
        if k<n { qh[k]=qd.eps[0]; } else { qi[k-n]=qd.eps[0]; }
    }
    (a,b,qh,qi)
}

struct LocalCallbacks;
impl<'a> BlockCallbacks<LocalAdvanceRequest<'a>> for LocalCallbacks {
    fn residual(&self,h:&[f64],input:&[f64],request:&LocalAdvanceRequest<'a>)->CaeResult<Vec<f64>> {Ok(evaluate(request,h,input).0)}
    fn state_jacobian(&self,h:&[f64],input:&[f64],request:&LocalAdvanceRequest<'a>)->CaeResult<Jacobian> {Ok(Jacobian::Dense(linearize(request,h,input,false).0))}
    fn design_jacobian(&self,h:&[f64],input:&[f64],request:&LocalAdvanceRequest<'a>)->CaeResult<Jacobian> {Ok(Jacobian::Dense(linearize(request,h,input,true).1))}
}

pub fn advance_local_history(r: &LocalAdvanceRequest<'_>, options: &LocalAdvanceOptions) -> CaeResult<LocalAdvanceResult> {
    let w=r.model.local_width();let n=r.model.internal_size;let ls=r.model.local_size();
    if r.driving.len()!=w || r.previous.len()!=w || n==0 || r.design.len()!=5 || !options.tolerance.is_finite() || options.tolerance<=0. || options.maximum_iterations==0 || !options.condition_limit.is_finite() || options.condition_limit<=1. {
        return Err(CaeError::contract("invalid local history advancement shape or numerical options"));
    }
    if r.model.creep.is_some_and(|c| c != crate::inelastic::CreepLaw::Norton) || r.model.plastic.is_some() || r.model.viscoelastic.is_some() || r.model.history.as_ref().is_some_and(|h| !matches!(&h.law, HistoryLaw::Species(_))) { return Err(CaeError::contract("local history split advancement currently supports creep and saturating-species history components only")); }
    let input:Vec<f64>=r.driving.iter().chain(r.previous).chain(r.design).copied().collect();
    if input.iter().any(|v|!v.is_finite()) || r.driving[..4].iter().chain(&r.previous[..4]).any(|t|r.model.t0+r.model.ts*t<=0.) || r.driving[ls]<0. || r.design[0]<0. || r.design[0]>1. || r.design[4]<0. || r.design[4]>1. || r.design[1..4].iter().any(|h|*h<=0.) { return Err(CaeError::contract("local history advancement requires finite inputs, positive temperatures and nonnegative interval")); }
    if let Some(history)=&r.model.history {
        let old=r.model.fields(r.reference_gradients,r.previous,r.design);
        history.check_state(&old.state[r.model.layout.material_start()..])?;
    }
    let initial=r.previous[16..16+n].to_vec();
    let solver=ImplicitBlockSystem::new(Arc::new(LocalCallbacks),BlockOptions{tolerance:options.tolerance,max_iterations:options.maximum_iterations,condition_limit:options.condition_limit,relaxed_tolerance:None,rejected_state_capture:options.rejected_state_capture.as_ref().map(|capture|(**capture).clone()),rejected_state_context:Some(Arc::new(|r:&LocalAdvanceRequest<'_>|{let mut frame=vec![r.model.t0,r.model.ts,r.model.es,r.model.ss,r.model.ls,r.model.ks,r.model.us];frame.extend_from_slice(&r.model.scales);for gradient in r.reference_gradients {frame.extend_from_slice(gradient);}frame})),..Default::default()})?;
    let solved=solver.solve(&input,&initial,r).map_err(|error|error.context("native local implicit history advancement"))?;
    let h=solved.state;
    let iterations=solved.iterations;
    let condition=solved.condition_number;
    if let Some(history)=&r.model.history {
        let mut current=r.driving.to_vec();current[16..16+n].copy_from_slice(&h);
        let fields=r.model.fields(r.reference_gradients,&current,r.design);
        history.check_state(&fields.state[r.model.layout.material_start()..])?;
    }
    let (a,b,qh,qi)=linearize(r,&h,&input,true);
    let lu=DenseLu::new(&a).map_err(|e|CaeError::convergence(e.to_string()))?;
    let mut derivative=DenseMatrix::zeros(n,input.len());
    for j in 0..input.len() {
        let rhs:Vec<f64>=(0..n).map(|i|-b.get(i,j)).collect();
        let column=lu.solve(&rhs,1,false).map_err(|e|CaeError::convergence(e.to_string()))?;
        for i in 0..n { derivative.data[i*input.len()+j]=column[i]; }
    }
    let heat_gradient:Vec<f64>=(0..input.len()).map(|j|qi[j]+(0..n).map(|i|qh[i]*derivative.get(i,j)).sum::<f64>()).collect();
    let (residual,heat)=evaluate(r,&h,&input);
    if !heat.is_finite() || heat<0. || derivative.data.iter().chain(&heat_gradient).any(|v|!v.is_finite()) { return Err(CaeError::convergence("nonfinite history tangent or negative dissipated heat")); }
    Ok(LocalAdvanceResult{normalized_history:h,integrated_heat_J_m3:heat,residual_norm:norm(&residual),iterations,local_condition:condition,input_width:input.len(),state_input_jacobian:derivative,heat_input_gradient:heat_gradient})
}

pub fn advance_local_substeps(r:&LocalAdvanceRequest<'_>,options:&LocalAdvanceOptions,substeps:usize)->CaeResult<LocalAdvanceResult> {
    if substeps==0 {return Err(CaeError::contract("local history substep count must be positive"));}
    let w=r.model.local_width();let ls=r.model.local_size();let n=r.model.internal_size;let m=2*w+r.design.len();
    if r.driving.len()!=w || r.previous.len()!=w || r.design.len()!=5 {return Err(CaeError::contract("local history substep shape mismatch"));}
    let mut previous=r.previous.to_vec();let mut history=r.previous[16..16+n].to_vec();
    let mut derivative=DenseMatrix::zeros(n,m);
    for i in 0..n {derivative.data[i*m+w+16+i]=1.;}
    let mut heat=0.;let mut heat_gradient=vec![0.;m];let mut iterations=0;let mut condition:f64=0.;let mut residual:f64=0.;
    for step in 0..substeps {
        let mut driving=r.driving.to_vec();driving[ls]/=substeps as f64;
        let mut map=DenseMatrix::zeros(m,m);
        for i in 0..w {map.data[i*m+i]=if i==ls {1./substeps as f64} else {1.};}
        for i in 0..w {map.data[(w+i)*m+if step==0 {w+i} else {i}]=1.;}
        for i in 0..n {
            for j in 0..m {map.data[(w+16+i)*m+j]=derivative.get(i,j);}
        }
        for i in 2*w..m {map.data[i*m+i]=1.;}
        previous[16..16+n].copy_from_slice(&history);
        let mut local_options=options.clone();
        if let Some(capture)=options.rejected_state_capture.as_ref() {let mut scoped=(**capture).clone();scoped.provenance=serde_json::json!({"caller":scoped.provenance,"local_substep_index":step,"local_substeps":substeps,"local_interval_s":driving[ls],"context_layout":{"normalization_prefix":["temperature_reference_K","temperature_scale_K","strain_scale","stress_scale_Pa","length_scale_m","conductivity_scale_W_mK","displacement_scale_m"],"internal_scale_count":r.model.scales.len(),"reference_gradient_rows":4,"reference_gradient_columns":3},"design_layout":{"driving_local_width":w,"previous_local_width":w,"physical_design_width":5}});local_options.rejected_state_capture=Some(Arc::new(scoped));}
        let result=advance_local_history(&LocalAdvanceRequest{model:r.model,reference_gradients:r.reference_gradients,driving:&driving,previous:&previous,design:r.design},&local_options).map_err(|error|error.context(&format!("local_substep={} of {}, interval_s={}",step,substeps,driving[ls])))?;
        let mut next=DenseMatrix::zeros(n,m);
        for j in 0..m {
            let direction:Vec<f64>=(0..m).map(|i|map.get(i,j)).collect();
            let (h,q)=result.apply(&direction)?;
            for i in 0..n {next.data[i*m+j]=h[i];}
            heat_gradient[j]+=q;
        }
        derivative=next;history=result.normalized_history;heat+=result.integrated_heat_J_m3;
        iterations+=result.iterations;condition=condition.max(result.local_condition);residual=residual.max(result.residual_norm);
        previous=r.driving.to_vec();
    }
    Ok(LocalAdvanceResult{normalized_history:history,integrated_heat_J_m3:heat,residual_norm:residual,iterations,local_condition:condition,input_width:m,state_input_jacobian:derivative,heat_input_gradient:heat_gradient})
}

pub fn endpoint_heat_replacement<S:Scalar>(model:&SolidModel,grad0:&[[f64;3];4],current:&[S],previous:&[S],design:&[S],integrated_heat_J_m3:S)->CaeResult<[S;4]> {
    let w=model.local_width();let ls=model.local_size();
    if current.len()!=w || previous.len()!=w || design.len()!=5 || current[ls].value()<=0. || !current[ls].value().is_finite() || !integrated_heat_J_m3.value().is_finite() || integrated_heat_J_m3.value()<0. || model.plastic.is_some() || model.viscoelastic.is_some() || model.history.as_ref().is_some_and(|h|!h.law.supports_closed_inventory_step()) {
        return Err(CaeError::contract("endpoint heat replacement requires finite positive interval and supported dissipative history"));
    }
    let forcing:Vec<f64>=current[ls+2..].iter().map(Scalar::value).collect();
    let (fields,endpoint)=model.observables(grad0,current,previous,design,current[ls],&forcing);
    let correction=-(integrated_heat_J_m3-endpoint.heat_increment)*fields.volume/(current[ls]*4.*(model.ks*model.ts*model.ls));
    if !correction.value().is_finite() {return Err(CaeError::convergence("nonfinite endpoint heat replacement"));}
    Ok([correction;4])
}
