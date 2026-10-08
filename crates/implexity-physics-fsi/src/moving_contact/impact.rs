// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_linalg::dense::{DenseLu,DenseMatrix};
fn fail(s:&str)->CaeError {CaeError::contract(s)}
fn dot(a:&[f64],b:&[f64])->f64 {a.iter().zip(b).map(|(a,b)|a*b).sum()}
pub trait MassMetric {
    fn size(&self)->usize;
    fn fixed(&self)->&[bool];
    fn apply(&self,v:&[f64])->CaeResult<Vec<f64>>;
    fn inverse_free(&self,force:&[f64])->CaeResult<Vec<f64>>;
}
#[derive(Clone,Copy)]
pub struct ImpactTolerance {
    pub event_gap_m:f64,
    pub normal_velocity_m_s:f64,
    pub impulse_n_s:f64,
    pub momentum_n_s:f64,
    pub energy_j:f64,
}
#[derive(Clone)]
pub struct Impact {
    pub velocity:Vec<f64>,
    pub impulses:Vec<f64>,
    pub normal_before:Vec<f64>,
    pub normal_after:Vec<f64>,
    pub active:Vec<usize>,
    pub support_impulse:Vec<f64>,
    pub kinetic_change_j:f64,
    pub contact_work_j:f64,
    pub support_work_j:f64,
    pub dissipated_j:f64,
    pub balance_defect_j:f64,
    tolerance:ImpactTolerance,
    inverse_rows:Vec<Vec<f64>>,
    delassus:DenseMatrix,
}
fn inputs<M:MassMetric>(mass:&M,v:&[f64],rows:&[Vec<f64>],e:&[f64])->CaeResult<()> {
    if v.len()!=mass.size() || mass.fixed().len()!=v.len() || rows.is_empty()
        || e.len()!=rows.len() || v.iter().any(|x|!x.is_finite())
        || rows.iter().any(|r|r.len()!=v.len() || r.iter().any(|x|!x.is_finite()))
        || e.iter().any(|x|!x.is_finite() || !(0. ..=1.).contains(x)) {
        return Err(fail("invalid impact dimensions or finite physical data"));
    }
    Ok(())
}
pub fn solve<M:MassMetric>(mass:&M,v:&[f64],rows:&[Vec<f64>],e:&[f64],gaps_m:&[f64],tolerance:ImpactTolerance)->CaeResult<Impact> {
    if rows.len()>12{return Err(fail("invalid impact dimensions or finite physical data"));}
    inputs(mass,v,rows,e)?;
    if ![tolerance.event_gap_m,tolerance.normal_velocity_m_s,tolerance.impulse_n_s,tolerance.momentum_n_s,tolerance.energy_j].iter().all(|x|x.is_finite() && *x>0.)
        || gaps_m.len()!=rows.len() || gaps_m.iter().any(|x|!x.is_finite() || x.abs()>tolerance.event_gap_m) {
        return Err(fail("invalid impact units or contact-event location"));
    }
    let n=rows.len();let before:Vec<_>=rows.iter().map(|r|dot(r,v)).collect();
    let inverse:Vec<_>=rows.iter().map(|r|mass.inverse_free(r)).collect::<CaeResult<_>>()?;
    if inverse.iter().any(|r|r.len()!=v.len() || r.iter().any(|x|!x.is_finite())) {return Err(fail("invalid inverse mass action"));}
    let mut a=DenseMatrix::zeros(n,n);
    for i in 0..n {for j in 0..n {a.data[i*n+j]=dot(&rows[i],&inverse[j]);}}
    if a.data.iter().any(|x|!x.is_finite()) || (0..n).any(|i|a.get(i,i)<=0.) {return Err(fail("unresponsive impact mass or overflow"));}
    let q:Vec<_>=before.iter().zip(e).map(|(w,e)|(1.+e)*w).collect();
    let mut chosen=None;
    for mask in 0..(1usize<<n) {
        let active:Vec<_>=(0..n).filter(|i|mask&(1<<i)!=0).collect();
        let mut impulse=vec![0.;n];
        if !active.is_empty() {
            let mut sub=DenseMatrix::zeros(active.len(),active.len());
            for (i,&r) in active.iter().enumerate(){for(j,&c)in active.iter().enumerate(){sub.data[i*active.len()+j]=a.get(r,c);}}
            let Ok(factor)=DenseLu::new(&sub) else {continue};
            let rhs:Vec<_>=active.iter().map(|&i|-q[i]).collect();
            let solved=factor.solve(&rhs,1,false).map_err(|x|fail(&x.to_string()))?;
            for (&i,&p) in active.iter().zip(&solved){impulse[i]=p;}
        }
        let y:Vec<_>=(0..n).map(|i|q[i]+(0..n).map(|j|a.get(i,j)*impulse[j]).sum::<f64>()).collect();
        if impulse.iter().any(|x|!x.is_finite() || *x < -tolerance.impulse_n_s)
            || y.iter().any(|x|!x.is_finite() || *x < -tolerance.normal_velocity_m_s) {continue;}
        if active.iter().any(|&i|y[i].abs()>tolerance.normal_velocity_m_s) {continue;}
        chosen=Some((impulse,active));break;
    }
    let (impulses,active)=chosen.ok_or_else(||fail("impact complementarity has no admitted active set"))?;
    if impulses.iter().any(|p|*p<0.) {return Err(fail("negative impact impulse within numerical tolerance"));}
    let delta:Vec<_>=(0..v.len()).map(|k|(0..n).map(|i|inverse[i][k]*impulses[i]).sum::<f64>()).collect();
    let velocity:Vec<_>=v.iter().zip(&delta).map(|(a,b)|a+b).collect();
    let after:Vec<_>=rows.iter().map(|r|dot(r,&velocity)).collect();
    let momentum=mass.apply(&delta)?;
    if momentum.len()!=v.len(){return Err(fail("impact momentum shape"));}
    let contact:Vec<_>=(0..v.len()).map(|k|(0..n).map(|i|rows[i][k]*impulses[i]).sum::<f64>()).collect();
    let support:Vec<_>=momentum.iter().zip(&contact).map(|(a,b)|a-b).collect();
    if support.iter().zip(mass.fixed()).any(|(r,f)|!f && r.abs()>tolerance.momentum_n_s) {return Err(fail("inverse mass momentum residual"));}
    if delta.iter().zip(mass.fixed()).any(|(d,f)|*f && *d!=0.) {return Err(fail("impact changed prescribed velocity"));}
    let midpoint:Vec<_>=v.iter().zip(&velocity).map(|(a,b)|0.5*(a+b)).collect();
    let kinetic=dot(&midpoint,&momentum);
    let contact_work=dot(&midpoint,&contact);let support_work=dot(&midpoint,&support);
    let dissipated=-contact_work;let defect=kinetic-contact_work-support_work;
    if velocity.iter().chain(&momentum).chain(&after).any(|x|!x.is_finite())
        || ![kinetic,contact_work,support_work,dissipated,defect].iter().all(|x|x.is_finite()) {
        return Err(fail("impact output overflow"));
    }
    if dissipated < -tolerance.energy_j {return Err(fail("multi-contact Newton restitution increased contact energy"));}
    Ok(Impact{velocity,impulses,normal_before:before,normal_after:after,active,support_impulse:support,
        kinetic_change_j:kinetic,contact_work_j:contact_work,support_work_j:support_work,dissipated_j:dissipated,balance_defect_j:defect,tolerance,inverse_rows:inverse,delassus:a})
}
pub fn direction<M:MassMetric>(mass:&M,base:&Impact,v:&[f64],rows:&[Vec<f64>],e:&[f64],
    dv:&[f64],drows:&[Vec<f64>],de:&[f64],dm:&dyn Fn(&[f64])->CaeResult<Vec<f64>>)->CaeResult<(Vec<f64>,Vec<f64>)> {
    inputs(mass,v,rows,e)?;inputs(mass,dv,drows,&vec![0.;e.len()])?;
    if de.len()!=e.len() || de.iter().any(|x|!x.is_finite()) {return Err(fail("invalid restitution direction"));}
    let n=rows.len();
    if base.velocity.len()!=v.len() || base.impulses.len()!=n || base.normal_before.len()!=n
        || base.normal_after.len()!=n || base.inverse_rows.len()!=n
        || base.inverse_rows.iter().any(|x|x.len()!=v.len())
        || base.delassus.nrows!=n || base.delassus.ncols!=n
        || base.active.iter().any(|i|*i>=n) {
        return Err(fail("impact derivative base shape"));
    }
    for i in 0..rows.len() {
        let y=base.normal_after[i]+e[i]*base.normal_before[i];
        if (base.impulses[i]>0. && y.abs()>base.tolerance.normal_velocity_m_s) || (base.impulses[i]==0. && y<=0.) {return Err(fail("impact derivative branch is not strict"));}
    }
    let n=rows.len();let mut dx=Vec::new();
    for i in 0..n {
        let mx=dm(&base.inverse_rows[i])?;
        if mx.len()!=mass.size() || mx.iter().any(|x|!x.is_finite()) {return Err(fail("invalid mass direction action"));}
        let rhs:Vec<_>=drows[i].iter().zip(mx).map(|(a,b)|a-b).collect();let x=mass.inverse_free(&rhs)?;if x.len()!=v.len() || x.iter().any(|a|!a.is_finite()){return Err(fail("invalid inverse mass direction"));}dx.push(x);
    }
    let mut da=DenseMatrix::zeros(n,n);
    for i in 0..n {for j in 0..n {da.data[i*n+j]=dot(&drows[i],&base.inverse_rows[j])+dot(&rows[i],&dx[j]);}}
    let dq:Vec<_>=(0..n).map(|i|(1.+e[i])*(dot(&drows[i],v)+dot(&rows[i],dv))+de[i]*base.normal_before[i]).collect();
    let active=&base.active;let mut dp=vec![0.;n];
    if !active.is_empty() {
        let mut sub=DenseMatrix::zeros(active.len(),active.len());
        for(i,&r)in active.iter().enumerate(){for(j,&c)in active.iter().enumerate(){sub.data[i*active.len()+j]=base.delassus.get(r,c);}}
        let rhs:Vec<_>=active.iter().map(|&i|-dq[i]-(0..n).map(|j|da.get(i,j)*base.impulses[j]).sum::<f64>()).collect();
        let solved=DenseLu::new(&sub).and_then(|f|f.solve(&rhs,1,false)).map_err(|x|fail(&x.to_string()))?;
        for(&i,&x)in active.iter().zip(&solved){dp[i]=x;}
    }
    let out:Vec<_>=(0..mass.size()).map(|k|dv[k]+(0..n).map(|i|dx[i][k]*base.impulses[i]+base.inverse_rows[i][k]*dp[i]).sum::<f64>()).collect();
    if out.iter().chain(&dp).any(|x|!x.is_finite()){return Err(fail("impact derivative overflow"));}
    Ok((out,dp))
}

pub struct ImpactBars {
    pub velocity: Vec<f64>,
    pub rows: Vec<Vec<f64>>,
    pub restitution: Vec<f64>,
    pub mass_pairs: Vec<(Vec<f64>, Vec<f64>)>,
}
pub fn adjoint<M:MassMetric>(mass:&M,base:&Impact,v:&[f64],rows:&[Vec<f64>],e:&[f64],bar_velocity:&[f64],bar_impulses:&[f64])->CaeResult<ImpactBars>{
    inputs(mass,v,rows,e)?;
    let n=rows.len();
    if bar_velocity.len()!=v.len() || bar_impulses.len()!=n || bar_velocity.iter().chain(bar_impulses).any(|x|!x.is_finite()) {
        return Err(fail("impact cotangent dimensions or finiteness"));
    }
    let zero=vec![0.;v.len()];
    direction(mass,base,v,rows,e,&zero,&vec![zero.clone();n],&vec![0.;n],&|_|Ok(zero.clone()))?;
    let mut bv=bar_velocity.to_vec();
    let mut br=vec![vec![0.;v.len()];n];
    let mut be=vec![0.;n];
    let mut bx:Vec<Vec<f64>>=base.impulses.iter().map(|p|bar_velocity.iter().map(|b|p*b).collect()).collect();
    let bp:Vec<f64>=(0..n).map(|i|bar_impulses[i]+dot(&base.inverse_rows[i],bar_velocity)).collect();
    let mut z=vec![0.;n];
    if !base.active.is_empty(){
        let a=&base.active;let mut sub=DenseMatrix::zeros(a.len(),a.len());
        for(i,&r)in a.iter().enumerate(){for(j,&c)in a.iter().enumerate(){sub.data[i*a.len()+j]=base.delassus.get(r,c);}}
        let rhs:Vec<_>=a.iter().map(|&i|bp[i]).collect();
        let solved=DenseLu::new(&sub).and_then(|f|f.solve(&rhs,1,true)).map_err(|x|fail(&x.to_string()))?;
        for(&i,&x)in a.iter().zip(&solved){z[i]=x;}
    }
    for i in 0..n {
        let bq=-z[i];be[i]=bq*base.normal_before[i];
        for k in 0..v.len(){bv[k]+=bq*(1.+e[i])*rows[i][k];br[i][k]+=bq*(1.+e[i])*v[k];}
        for j in 0..n {
            let ba=-z[i]*base.impulses[j];
            for k in 0..v.len(){br[i][k]+=ba*base.inverse_rows[j][k];bx[j][k]+=ba*rows[i][k];}
        }
    }
    let mut mass_pairs:Vec<(Vec<f64>,Vec<f64>)>=Vec::with_capacity(n);
    for i in 0..n {
        let y=mass.inverse_free(&bx[i])?;
        if y.len()!=v.len() || y.iter().any(|x|!x.is_finite()){return Err(fail("impact inverse mass cotangent"));}
        for k in 0..v.len(){br[i][k]+=y[k];}
        mass_pairs.push((y.iter().map(|x|-x).collect(),base.inverse_rows[i].clone()));
    }
    if bv.iter().chain(br.iter().flatten()).chain(&be).chain(mass_pairs.iter().flat_map(|(a,b)|a.iter().chain(b))).any(|x|!x.is_finite()){
        return Err(fail("impact adjoint overflow"));
    }
    Ok(ImpactBars{velocity:bv,rows:br,restitution:be,mass_pairs})
}

#[derive(Clone,Copy)]
pub struct ActiveSetImpactPolicy{pub maximum_pivots:usize}
#[derive(Clone)]
pub struct ActiveSetImpactReport{pub features:usize,pub pivots:usize,pub removals:usize,pub restricted_solves:usize,pub symmetry_relative_error:f64,pub minimum_normalized_cholesky_pivot:f64,pub maximum_negative_slack_m_s:f64,pub maximum_active_slack_m_s:f64}
fn positive_delassus(a:&DenseMatrix)->CaeResult<(f64,f64)>{
 let n=a.nrows;if n==0||a.ncols!=n||a.data.iter().any(|v|!v.is_finite())||(0..n).any(|i|a.get(i,i)<=0.){return Err(fail("invalid physical Delassus matrix"));}let roundoff=64.*f64::EPSILON*n as f64;let mut symmetry=0_f64;let mut l=vec![0.;n.checked_mul(n).ok_or_else(||fail("Delassus dimension overflow"))?];let scales:Vec<_>=(0..n).map(|i|a.get(i,i).sqrt()).collect();let mut minimum=f64::INFINITY;
 for i in 0..n{for j in 0..=i{let x=a.get(i,j)/scales[i]/scales[j];let y=a.get(j,i)/scales[i]/scales[j];symmetry=symmetry.max((x-y).abs());if !x.is_finite()||!y.is_finite()||symmetry>roundoff{return Err(fail("physical mass inverse Delassus symmetry uncertified"));}let mut v=0.5*(x+y);for k in 0..j{v-=l[i*n+k]*l[j*n+k];}if i==j{if !v.is_finite()||v<=roundoff{return Err(fail("dependent or numerically unresolved impact feature rank"));}minimum=minimum.min(v);l[i*n+i]=v.sqrt();}else{l[i*n+j]=v/l[j*n+j];if !l[i*n+j].is_finite(){return Err(fail("Delassus rank certification overflow"));}}}}
 Ok((symmetry,minimum))
}
fn passive_delassus(a:&DenseMatrix,q:&[f64],tolerance:ImpactTolerance,policy:ActiveSetImpactPolicy)->CaeResult<(Vec<f64>,Vec<usize>,usize,usize,usize)>{
 let n=q.len();if policy.maximum_pivots==0||a.nrows!=n||a.ncols!=n||q.iter().any(|v|!v.is_finite()){return Err(fail("impact active-set policy/system shape"));}let mut x=vec![0.;n];let mut passive=vec![false;n];let(mut pivots,mut removals,mut solves)=(0,0,0);let mut seen=std::collections::BTreeSet::new();
 loop{let y:Vec<_>=(0..n).map(|i|q[i]+(0..n).map(|j|a.get(i,j)*x[j]).sum::<f64>()).collect();if y.iter().any(|v|!v.is_finite()){return Err(fail("impact active-set slack overflow"));}let entering=(0..n).filter(|i|!passive[*i]&&y[*i]< -tolerance.normal_velocity_m_s).min_by(|i,j|y[*i].total_cmp(&y[*j]).then(i.cmp(j)));let Some(entering)=entering else{let active:Vec<_>=(0..n).filter(|i|passive[*i]).collect();if x.iter().any(|v|!v.is_finite()||*v<0.)||y.iter().any(|v|*v< -tolerance.normal_velocity_m_s)||active.iter().any(|i|y[*i].abs()>tolerance.normal_velocity_m_s){return Err(fail("impact active-set complementarity residual uncertified"));}return Ok((x,active,pivots,removals,solves));};if pivots>=policy.maximum_pivots{return Err(fail("impact active-set pivot budget exhausted"));}passive[entering]=true;pivots+=1;
 loop{if solves>=policy.maximum_pivots{return Err(fail("impact active-set restricted solve budget exhausted"));}let active:Vec<_>=(0..n).filter(|i|passive[*i]).collect();let key=(active.clone(),x.iter().map(|v:&f64|v.to_bits()).collect::<Vec<_>>());if !seen.insert(key){return Err(fail("impact active-set cycling refused"));}let mut sub=DenseMatrix::zeros(active.len(),active.len());for(i,&r)in active.iter().enumerate(){for(j,&c)in active.iter().enumerate(){sub.data[i*active.len()+j]=a.get(r,c);}}let rhs:Vec<_>=active.iter().map(|i|-q[*i]).collect();let zsmall=DenseLu::new(&sub).and_then(|f|f.solve(&rhs,1,false)).map_err(|_|fail("impact active restricted Delassus is singular"))?;solves+=1;let mut z=vec![0.;n];for(&i,&v)in active.iter().zip(&zsmall){z[i]=v;}if z.iter().any(|v|!v.is_finite()){return Err(fail("impact active-set solution overflow"));}if active.iter().all(|i|z[*i]>=0.){x=z;break;}let mut alpha=1_f64;let mut blockers=vec![];for &i in &active{if z[i]<0.{let ratio=x[i]/(x[i]-z[i]);if !ratio.is_finite()||!(0. ..=1.).contains(&ratio){return Err(fail("impact feasible segment ratio invalid"));}if ratio<alpha{alpha=ratio;blockers.clear();blockers.push(i);}else if ratio==alpha{blockers.push(i);}}}if blockers.is_empty(){return Err(fail("impact infeasible passive set has no blocking variable"));}for i in 0..n{x[i]+=alpha*(z[i]-x[i]);}for i in blockers{x[i]=0.;passive[i]=false;removals+=1;}if x.iter().any(|v|!v.is_finite()||*v<0.){return Err(fail("impact feasible segment lost nonnegativity"));}}
 }
}
pub fn solve_scalable<M:MassMetric>(mass:&M,v:&[f64],rows:&[Vec<f64>],e:&[f64],gaps_m:&[f64],tolerance:ImpactTolerance,policy:ActiveSetImpactPolicy)->CaeResult<Impact>{Ok(solve_scalable_with_report(mass,v,rows,e,gaps_m,tolerance,policy)?.0)}
pub fn solve_scalable_with_report<M:MassMetric>(mass:&M,v:&[f64],rows:&[Vec<f64>],e:&[f64],gaps_m:&[f64],tolerance:ImpactTolerance,policy:ActiveSetImpactPolicy)->CaeResult<(Impact,ActiveSetImpactReport)>{
    inputs(mass,v,rows,e)?;
    if ![tolerance.event_gap_m,tolerance.normal_velocity_m_s,tolerance.impulse_n_s,tolerance.momentum_n_s,tolerance.energy_j].iter().all(|x|x.is_finite() && *x>0.)
        || gaps_m.len()!=rows.len() || gaps_m.iter().any(|x|!x.is_finite() || x.abs()>tolerance.event_gap_m) {
        return Err(fail("invalid impact units or contact-event location"));
    }
    rows.len().checked_mul(rows.len()).ok_or_else(||fail("impact feature matrix dimension overflow"))?;
    let n=rows.len();let before:Vec<_>=rows.iter().map(|r|dot(r,v)).collect();
    let inverse:Vec<_>=rows.iter().map(|r|mass.inverse_free(r)).collect::<CaeResult<_>>()?;
    if inverse.iter().any(|r|r.len()!=v.len() || r.iter().any(|x|!x.is_finite())) {return Err(fail("invalid inverse mass action"));}
    let mut a=DenseMatrix::zeros(n,n);
    for i in 0..n {for j in 0..n {a.data[i*n+j]=dot(&rows[i],&inverse[j]);}}
    if a.data.iter().any(|x|!x.is_finite()) || (0..n).any(|i|a.get(i,i)<=0.) {return Err(fail("unresponsive impact mass or overflow"));}
    let q:Vec<_>=before.iter().zip(e).map(|(w,e)|(1.+e)*w).collect();
    let(symmetry,minimum)=positive_delassus(&a)?;
    let(impulses,active,pivots,removals,restricted_solves)=passive_delassus(&a,&q,tolerance,policy)?;
    let slack:Vec<_>=(0..n).map(|i|q[i]+(0..n).map(|j|a.get(i,j)*impulses[j]).sum::<f64>()).collect();
    let report=ActiveSetImpactReport{features:n,pivots,removals,restricted_solves,symmetry_relative_error:symmetry,minimum_normalized_cholesky_pivot:minimum,maximum_negative_slack_m_s:slack.iter().map(|v|(-v).max(0.)).fold(0.,f64::max),maximum_active_slack_m_s:active.iter().map(|i|slack[*i].abs()).fold(0.,f64::max)};
    if impulses.iter().any(|p|*p<0.) {return Err(fail("negative impact impulse within numerical tolerance"));}
    let delta:Vec<_>=(0..v.len()).map(|k|(0..n).map(|i|inverse[i][k]*impulses[i]).sum::<f64>()).collect();
    let velocity:Vec<_>=v.iter().zip(&delta).map(|(a,b)|a+b).collect();
    let after:Vec<_>=rows.iter().map(|r|dot(r,&velocity)).collect();
    if after.iter().any(|w|*w < -tolerance.normal_velocity_m_s){return Err(fail("impact restitution left a closing contact normal"));}
    let momentum=mass.apply(&delta)?;
    if momentum.len()!=v.len(){return Err(fail("impact momentum shape"));}
    let contact:Vec<_>=(0..v.len()).map(|k|(0..n).map(|i|rows[i][k]*impulses[i]).sum::<f64>()).collect();
    let support:Vec<_>=momentum.iter().zip(&contact).map(|(a,b)|a-b).collect();
    if support.iter().zip(mass.fixed()).any(|(r,f)|!f && r.abs()>tolerance.momentum_n_s) {return Err(fail("inverse mass momentum residual"));}
    if delta.iter().zip(mass.fixed()).any(|(d,f)|*f && *d!=0.) {return Err(fail("impact changed prescribed velocity"));}
    let midpoint:Vec<_>=v.iter().zip(&velocity).map(|(a,b)|0.5*(a+b)).collect();
    let kinetic=dot(&midpoint,&momentum);
    let contact_work=dot(&midpoint,&contact);let support_work=dot(&midpoint,&support);
    let dissipated=-contact_work;let defect=kinetic-contact_work-support_work;
    if velocity.iter().chain(&momentum).chain(&after).any(|x|!x.is_finite())
        || ![kinetic,contact_work,support_work,dissipated,defect].iter().all(|x|x.is_finite()) {
        return Err(fail("impact output overflow"));
    }
    if dissipated < -tolerance.energy_j {return Err(fail("multi-contact Newton restitution increased contact energy"));}
    Ok((Impact{velocity,impulses,normal_before:before,normal_after:after,active,support_impulse:support,
        kinetic_change_j:kinetic,contact_work_j:contact_work,support_work_j:support_work,dissipated_j:dissipated,balance_defect_j:defect,tolerance,inverse_rows:inverse,delassus:a},report))
}
