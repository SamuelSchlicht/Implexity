// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::sync::{Arc,Mutex,MutexGuard};
use implexity_core::error::{CaeError,CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_physics_solid::history::local_snapshot::NativeLocalSnapshot;
use implexity_solve::implicit_block::{BlockCallbacks,BlockOptions,ImplicitBlockSystem,ImplicitSolveResult};
use implexity_solve::local_assembly::Kind;
use implexity_solve::local_condensation::LocalEliminationPartition;
use implexity_solve::matrix::Jacobian;
use implexity_solve::linear_workspace::{CertifiedFactorization,ExactFactorizationWorkspace,TransactionOptions};
use super::UnifiedKernel;

pub struct NativeSplitSnapshotMap {
    pub kernel:Arc<UnifiedKernel>,
    pub step:usize,
    pub previous:Vec<f64>,
    pub local:Arc<NativeLocalSnapshot>,
    pub internal_rows:Vec<(usize,usize,usize)>,
    pub partition:Arc<LocalEliminationPartition>,
    pub operation:Option<implexity_solve::operation_context::OperationExecutionContext>,
    linear_cache:Mutex<SnapshotFactorCache>,
}

struct SnapshotFactorCache {
    kernel:Arc<UnifiedKernel>,
    local:Arc<NativeLocalSnapshot>,
    forward:ExactFactorizationWorkspace,
    partition:Arc<LocalEliminationPartition>,
    condition_limit:u64,
    reverse:Option<(Vec<u64>,Arc<dyn CertifiedFactorization>,CsrMatrix,CsrMatrix)>,
}

fn csr(rows:usize,cols:usize,entries:&[(usize,usize,f64)])->CaeResult<CsrMatrix> {
    let rr:Vec<usize>=entries.iter().map(|e|e.0).collect();let cc:Vec<usize>=entries.iter().map(|e|e.1).collect();let vv:Vec<f64>=entries.iter().map(|e|e.2).collect();
    CsrMatrix::from_triplets(rows,cols,&rr,&cc,&vv).map_err(|e|CaeError::contract(e.to_string()))
}
impl NativeSplitSnapshotMap {
    pub fn new(kernel:Arc<UnifiedKernel>,step:usize,previous:Vec<f64>,local:Arc<NativeLocalSnapshot>)->CaeResult<Self> {
        if step==0 || step>=kernel.nt || previous.len()!=kernel.state_size || local.normalized_state.len()!=kernel.s.state_size || kernel.s.internal_size==0 {return Err(CaeError::contract("native split snapshot binding invalid"));}
        let mut rows=Vec::new();let mut groups=Vec::new();
        for e in 0..kernel.s.ne {
            let mut group=Vec::new();
            for i in 0..kernel.s.internal_size {
                let solid=kernel.s.n_t()+kernel.s.n_u()+e*kernel.s.internal_size+i;let full=kernel.solid_slice.start+solid;
                let (indices,values)=kernel.p_map.row(full);
                if indices.len()!=1 || values!=[1.] || kernel.offsets[step][full]!=0. {return Err(CaeError::contract("split history internal row must be retained with unit normalization"));}
                let reduced=indices[0];let (ri,rv)=kernel.w_map.row(reduced);
                if ri!=[full] || rv!=[1.] {return Err(CaeError::contract("split history internal equation must be retained without aggregation"));}
                rows.push((full,reduced,solid));group.push(reduced);
            }
            groups.push(group);
        }
        let partition=Arc::new(LocalEliminationPartition::new(kernel.state_size,groups).map_err(|e|CaeError::contract(e.to_string()))?);
        let linear_cache=Mutex::new(SnapshotFactorCache{kernel:Arc::clone(&kernel),local:Arc::clone(&local),forward:ExactFactorizationWorkspace::new(),partition:Arc::clone(&partition),condition_limit:kernel.reduction.system().options().condition_limit.to_bits(),reverse:None});
        Ok(Self{kernel,step,previous,local,internal_rows:rows,partition,operation:None,linear_cache})
    }
    fn factor_cache(&self)->CaeResult<MutexGuard<'_,SnapshotFactorCache>> {
        let mut cache=self.linear_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let limit=self.kernel.reduction.system().options().condition_limit.to_bits();
        if !Arc::ptr_eq(&cache.kernel,&self.kernel) || !Arc::ptr_eq(&cache.local,&self.local) || (!Arc::ptr_eq(&cache.partition,&self.partition) && cache.partition!=self.partition) || cache.condition_limit!=limit {
            cache.forward.clear()?;cache.reverse=None;cache.kernel=Arc::clone(&self.kernel);cache.local=Arc::clone(&self.local);cache.partition=Arc::clone(&self.partition);cache.condition_limit=limit;
        }
        Ok(cache)
    }
    pub fn residual(&self,current:&[f64],design:&[f64])->CaeResult<Vec<f64>> {
        let k=&self.kernel;let full=k.expand(self.step,current)?;let old=k.expand(self.step-1,&self.previous)?;
        let mut out=k.assembly.residual(self.step,&full,&old,design)?;
        let correction=self.local.heat_correction(self.step,&full[k.solid_slice.clone()],&old[k.solid_slice.clone()],design)?;
        for (i,value) in correction.iter().enumerate() {out[k.solid_slice.start+i]+=value;}
        for (row,_,solid) in &self.internal_rows {out[*row]=full[*row]-self.local.normalized_state[*solid];}
        k.w_map.matvec(&out).map_err(|e|CaeError::contract(e.to_string()))
    }
    pub fn jacobian(&self,kind:Kind,current:&[f64],design:&[f64])->CaeResult<CsrMatrix> {
        let k=&self.kernel;let full=k.expand(self.step,current)?;let old=k.expand(self.step-1,&self.previous)?;
        let original=k.assembly.jacobian(kind,self.step,&full,&old,design)?;
        let correction=self.local.heat_correction_jacobian(kind,self.step,&full[k.solid_slice.clone()],&old[k.solid_slice.clone()],design)?;
        let mut pinned=vec![false;k.full_size];for (row,_,_) in &self.internal_rows {pinned[*row]=true;}
        let mut entries=Vec::new();
        for row in 0..original.nrows() {if !pinned[row] {let (indices,values)=original.row(row);for (column,value) in indices.iter().zip(values) {if *value!=0. {entries.push((row,*column,*value));}}}}
        for row in 0..correction.nrows() {let (indices,values)=correction.row(row);for (column,value) in indices.iter().zip(values) {if *value!=0. {entries.push((k.solid_slice.start+row,if matches!(kind,Kind::Design) {*column} else {k.solid_slice.start+column},*value));}}}
        if matches!(kind,Kind::Current) {for (row,_,_) in &self.internal_rows {entries.push((*row,*row,1.));}}
        let corrected=csr(k.full_size,original.ncols(),&entries)?;
        let restricted=k.w_map.matmul(&corrected).map_err(|e|CaeError::contract(e.to_string()))?;
        if matches!(kind,Kind::Design) {Ok(restricted)} else {restricted.matmul(&k.p_map).map_err(|e|CaeError::contract(e.to_string()))}
    }
    pub fn solve(&self,driving:&[f64],design:&[f64])->CaeResult<ImplicitSolveResult> {
        if driving.len()!=self.kernel.state_size {return Err(CaeError::contract("native split snapshot initial guess shape mismatch"));}
        let mut initial=driving.to_vec();for (_,reduced,solid) in &self.internal_rows {initial[*reduced]=self.local.normalized_state[*solid];}
        let cache=self.factor_cache()?;
        let authored=self.kernel.reduction.system().options();
        let options=BlockOptions{tolerance:authored.tolerance,max_iterations:authored.max_iterations,condition_limit:authored.condition_limit,relaxed_tolerance:None,execution_context:self.operation.clone(),trace_context:self.operation.clone(),local_elimination_partition:Some(Arc::clone(&self.partition)),..Default::default()};
        let system=ImplicitBlockSystem::new(Arc::new(SnapshotCallbacks),options)?;
        system.install_exact_factorization_workspace(cache.forward.clone())?;
        let solved=system.solve(design,&initial,self)?;
        if let Some(mut staged)=system.take_staged_exact_factorization() {staged.commit()?;}
        Ok(solved)
    }
    pub fn local_output_adjoint(&self,current:&[f64],design:&[f64],adjoint:&[f64])->CaeResult<(Vec<f64>,Vec<f64>)> {
        let k=&self.kernel;let full=k.expand(self.step,current)?;let old=k.expand(self.step-1,&self.previous)?;let full_adjoint=k.w_map.matvec_transpose(adjoint).map_err(|e|CaeError::contract(e.to_string()))?;
        self.local.local_output_adjoint(self.step,&full[k.solid_slice.clone()],&old[k.solid_slice.clone()],design,&full_adjoint[k.solid_slice.clone()])
    }
}
struct SnapshotCallbacks;
impl BlockCallbacks<NativeSplitSnapshotMap> for SnapshotCallbacks {
    fn residual(&self,state:&[f64],design:&[f64],context:&NativeSplitSnapshotMap)->CaeResult<Vec<f64>> {context.residual(state,design)}
    fn state_jacobian(&self,state:&[f64],design:&[f64],context:&NativeSplitSnapshotMap)->CaeResult<Jacobian> {Ok(Jacobian::Csr(context.jacobian(Kind::Current,state,design)?))}
    fn design_jacobian(&self,state:&[f64],design:&[f64],context:&NativeSplitSnapshotMap)->CaeResult<Jacobian> {Ok(Jacobian::Csr(context.jacobian(Kind::Design,state,design)?))}
}

pub struct SnapshotPullback {
    pub previous:Vec<f64>,
    pub driving:Vec<f64>,
    pub design:Vec<f64>,
    pub condition:f64,
    pub transpose_residual:f64,
    pub transpose_relative_residual:f64,
}
impl NativeSplitSnapshotMap {
    pub fn pullback(&self,current:&[f64],design:&[f64],state_seed:&[f64],local_heat_seed:&[f64])->CaeResult<SnapshotPullback> {
        self.pullback_block(current,design,&[state_seed.to_vec()],&[local_heat_seed.to_vec()])?.pop().ok_or_else(||CaeError::contract("native split pullback missing column"))
    }
    pub fn pullback_block(&self,current:&[f64],design:&[f64],state_seeds:&[Vec<f64>],local_heat_seeds:&[Vec<f64>])->CaeResult<Vec<SnapshotPullback>> {
        use implexity_solve::factorization::Factorization;
        let k=&self.kernel;let m=state_seeds.len();
        if local_heat_seeds.len()!=m || local_heat_seeds.iter().any(|v|v.len()!=k.s.ne || v.iter().any(|x|!x.is_finite())) || state_seeds.iter().any(|v|v.len()!=k.state_size || v.iter().any(|x|!x.is_finite())) {return Err(CaeError::contract("native split reverse block inputs invalid"));}
        if m==0 {return Ok(Vec::new());}
        let mut key=vec![current.len() as u64,design.len() as u64,self.previous.len() as u64,self.step as u64,self.internal_rows.len() as u64];
        key.extend(current.iter().chain(design).chain(&self.previous).map(|v|v.to_bits()));
        key.extend(self.internal_rows.iter().flat_map(|&(a,b,c)|[a as u64,b as u64,c as u64]));
        let mut cached=self.factor_cache()?;
        if cached.reverse.as_ref().is_none_or(|v|v.0!=key) {
            let a=self.jacobian(Kind::Current,current,design)?;
            let mut staged=cached.forward.transaction(Jacobian::Csr(a),k.state_size,k.reduction.system().options().condition_limit,|a|Ok(Arc::new(Factorization::new_with_partition(a,k.state_size,k.reduction.system().options().condition_limit,None,Some(&self.partition))?) as Arc<dyn CertifiedFactorization>),TransactionOptions{discard_committed_on_miss:true,require_committed_match:false})?;
            let factor=staged.factorization()?;
            let b=self.jacobian(Kind::Previous,current,design)?;let c=self.jacobian(Kind::Design,current,design)?;
            staged.commit()?;cached.reverse=Some((key,factor,b,c));
        }
        let (_,factor,b,c)=cached.reverse.as_ref().unwrap();
        let mut rhs=vec![0.;k.state_size*m];for i in 0..k.state_size {for j in 0..m {rhs[i*m+j]=state_seeds[j][i];}}
        let solved=factor.solve_block(&rhs,m,true)?;let mut out=Vec::with_capacity(m);
        for j in 0..m {
            let adjoint:Vec<f64>=(0..k.state_size).map(|i|solved.solution[i*m+j]).collect();
            let mut previous=b.matvec_transpose(&adjoint).map_err(|e|CaeError::contract(e.to_string()))?;
            let mut dx=c.matvec_transpose(&adjoint).map_err(|e|CaeError::contract(e.to_string()))?;
            previous.iter_mut().for_each(|v|*v=-*v);dx.iter_mut().for_each(|v|*v=-*v);
            let (mut hs,mut qs)=self.local_output_adjoint(current,design,&adjoint)?;hs.iter_mut().for_each(|v|*v=-*v);for (v,direct) in qs.iter_mut().zip(&local_heat_seeds[j]) {*v=-*v+direct;}
            let (local_driving,local_previous,local_design)=self.local.apply_transpose(&hs,&qs)?;
            let mut full_driving=vec![0.;k.full_size];let mut full_previous=vec![0.;k.full_size];
            full_driving[k.solid_slice.clone()].copy_from_slice(&local_driving);full_previous[k.solid_slice.clone()].copy_from_slice(&local_previous);
            let driving=k.reduce_cotangent(&full_driving)?;let previous_addition=k.reduce_cotangent(&full_previous)?;
            for (a,b) in previous.iter_mut().zip(previous_addition) {*a+=b;}
            for (a,b) in dx.iter_mut().zip(local_design) {*a+=b;}
            out.push(SnapshotPullback{previous,driving,design:dx,condition:factor.condition(),transpose_residual:solved.error_norms[j],transpose_relative_residual:solved.relative[j]});
        }
        Ok(out)
    }
}

pub struct NativeSplitSweep {
    pub map:NativeSplitSnapshotMap,
    pub solved:ImplicitSolveResult,
}
pub struct NativeSplitTrajectory {
    pub kernel:Arc<UnifiedKernel>,
    pub design:Vec<f64>,
    pub states:Vec<Vec<f64>>,
    pub sweeps:Vec<Vec<NativeSplitSweep>>,
    pub identity:serde_json::Value,
}
impl NativeSplitTrajectory {
    pub fn pullback(&self,state_seeds:&[Vec<f64>],direct_design:&[f64])->CaeResult<Vec<f64>> {
        self.pullback_with_heat(state_seeds,direct_design,&vec![vec![0.;self.kernel.s.ne];self.sweeps.len()])
    }
    pub fn pullback_with_heat(&self,state_seeds:&[Vec<f64>],direct_design:&[f64],interval_heat_seeds:&[Vec<f64>])->CaeResult<Vec<f64>> {
        self.pullback_block(&[state_seeds.to_vec()],&[direct_design.to_vec()],&[interval_heat_seeds.to_vec()])?.pop().ok_or_else(||CaeError::contract("native split trajectory missing column"))
    }
    pub fn pullback_block(&self,state_seeds:&[Vec<Vec<f64>>],direct_design:&[Vec<f64>],interval_heat_seeds:&[Vec<Vec<f64>>])->CaeResult<Vec<Vec<f64>>> {
        let m=state_seeds.len();
        if direct_design.len()!=m || interval_heat_seeds.len()!=m || interval_heat_seeds.iter().any(|s|s.len()!=self.sweeps.len() || s.iter().any(|v|v.len()!=self.kernel.s.ne || v.iter().any(|x|!x.is_finite()))) || state_seeds.iter().any(|s|s.len()!=self.states.len() || s.iter().any(|v|v.len()!=self.kernel.state_size || v.iter().any(|x|!x.is_finite()))) || direct_design.iter().any(|v|v.len()!=self.design.len() || v.iter().any(|x|!x.is_finite())) {return Err(CaeError::contract("native split trajectory reverse inputs invalid"));}
        if m==0 {return Ok(Vec::new());}
        let mut gradients=direct_design.to_vec();let mut seeds=state_seeds.to_vec();
        for step in (1..self.states.len()).rev() {
            let mut current:Vec<Vec<f64>>=seeds.iter().map(|s|s[step].clone()).collect();
            for (index,sweep) in self.sweeps[step-1].iter().enumerate().rev() {
                let heat:Vec<Vec<f64>>=interval_heat_seeds.iter().map(|s|if index+1==self.sweeps[step-1].len() {s[step-1].clone()} else {vec![0.;self.kernel.s.ne]}).collect();
                let pulls=sweep.map.pullback_block(&sweep.solved.state,&self.design,&current,&heat)?;
                for (j,pull) in pulls.into_iter().enumerate() {
                    for (a,b) in gradients[j].iter_mut().zip(pull.design) {*a+=b;}
                    for (a,b) in seeds[j][step-1].iter_mut().zip(pull.previous) {*a+=b;}
                    current[j]=pull.driving;
                }
            }
            for j in 0..m {for (a,b) in seeds[j][step-1].iter_mut().zip(&current[j]) {*a+=b;}}
        }
        for j in 0..m {
            let covectors=implexity_linalg::dense::DenseMatrix::new(self.kernel.state_size,1,seeds[j][0].clone()).map_err(|e|CaeError::contract(e.to_string()))?;
            let addition=self.kernel.reduction.initial_pullback(&self.design,&covectors)?;
            for (a,b) in gradients[j].iter_mut().zip(addition.data) {*a+=b;}
        }
        if gradients.iter().flatten().any(|v|!v.is_finite()) {return Err(CaeError::convergence("nonfinite native split history gradient"));}
        Ok(gradients)
    }
}

pub enum NativeDeclaredHistory {
    OriginalExact(implexity_solve::native_history::HistorySolution),
    DeclaredApproximate(NativeSplitTrajectory),
}

pub fn solve_declared_native_history(kernel:Arc<UnifiedKernel>,design:&[f64],options:&super::split_history::SplitHistoryOptions,local_options:&implexity_physics_solid::history::local_advance::LocalAdvanceOptions,context:Option<&implexity_solve::operation_context::OperationExecutionContext>)->CaeResult<NativeDeclaredHistory> {
    options.validate()?;
    if options.snapshot_times_s!=kernel.s.times {return Err(CaeError::contract("declared native history retains the original observation times"));}
    let model=&kernel.s.model;
    if model.creep.is_none() && model.history.is_none() && model.plastic.is_none() && model.viscoelastic.is_none() {return kernel.solve(design,context).map(NativeDeclaredHistory::OriginalExact);}
    solve_native_split_history_with_context(kernel,design,options,local_options,context).map(NativeDeclaredHistory::DeclaredApproximate)
}

pub fn solve_native_split_history(kernel:Arc<UnifiedKernel>,design:&[f64],options:&super::split_history::SplitHistoryOptions,local_options:&implexity_physics_solid::history::local_advance::LocalAdvanceOptions)->CaeResult<NativeSplitTrajectory> {
    solve_native_split_history_with_context(kernel,design,options,local_options,None)
}

pub fn solve_native_split_history_with_context(kernel:Arc<UnifiedKernel>,design:&[f64],options:&super::split_history::SplitHistoryOptions,local_options:&implexity_physics_solid::history::local_advance::LocalAdvanceOptions,context:Option<&implexity_solve::operation_context::OperationExecutionContext>)->CaeResult<NativeSplitTrajectory> {
    use implexity_physics_solid::history::local_snapshot::advance_native_snapshot;
    options.validate()?;
    if options.snapshot_times_s!=kernel.s.times || design.len()!=2*kernel.nc+3 || design.iter().any(|v|!v.is_finite()) {return Err(CaeError::contract("native split history requires original observation times and finite physical design"));}
    if options.aged_condition {return solve_native_age_condition(kernel,design,options,context);}
    let operation=kernel.canonical_context(context,"declared_operator_split_history")?;
    let initial=kernel.reduction.initial_for(design)?;let mut states=vec![initial];let mut intervals=Vec::new();
    for step in 1..kernel.nt {
        implexity_solve::trace::point("split_history.snapshot_started",||{let mut fields=operation.trace_fields();fields.insert("physical_step".into(),serde_json::json!(step));fields.insert("physical_time_s".into(),serde_json::json!(kernel.s.times[step]));fields.insert("history_method".into(),serde_json::json!("declared_operator_split"));fields})?;
        let previous=states[step-1].clone();let old_full=kernel.expand(step-1,&previous)?;let mut driving=previous.clone();let mut sweeps=Vec::new();
        for sweep in 0..options.coupling_sweeps {
            let driving_full=kernel.expand(step,&driving)?;
            let local=Arc::new(advance_native_snapshot(&kernel.s,step,&driving_full[kernel.solid_slice.clone()],&old_full[kernel.solid_slice.clone()],design,local_options,options.local_substeps)?);
            let mut map=NativeSplitSnapshotMap::new(Arc::clone(&kernel),step,previous.clone(),local)?;map.operation=Some(operation.clone());
            implexity_solve::trace::point("split_history.local_completed",||{let mut fields=operation.trace_fields();fields.insert("physical_step".into(),serde_json::json!(step));fields.insert("local_iterations".into(),serde_json::json!(map.local.local_iterations));fields.insert("local_residual_norm".into(),serde_json::json!(map.local.maximum_local_residual));fields.insert("local_condition".into(),serde_json::json!(map.local.maximum_local_condition));fields})?;
            let solved=map.solve(&driving,design).map_err(|error|error.context(&format!("native split equilibrium physical_step={}, time_s={}, sweep={}",step,kernel.s.times[step],sweep)))?;driving=solved.state.clone();sweeps.push(NativeSplitSweep{map,solved});
        }
        states.push(driving);intervals.push(sweeps);
    }
    Ok(NativeSplitTrajectory{kernel:Arc::clone(&kernel),design:design.to_vec(),states,sweeps:intervals,identity:serde_json::json!({"approximation":options.identity(),"physical_problem_sha256":implexity_core::json::canonical_sha256(&kernel.p),"local_numerics":{"tolerance":local_options.tolerance,"maximum_iterations":local_options.maximum_iterations,"condition_limit":local_options.condition_limit},"global_snapshot_solves":(kernel.nt-1)*options.coupling_sweeps,"initialization_auxiliary_solves":0,"local_advancements":(kernel.nt-1)*options.coupling_sweeps*options.local_substeps*kernel.s.ne})})
}

impl NativeSplitTrajectory {
    pub fn integrated_heat_value(&self)->CaeResult<f64> {
        let k=&self.kernel;let mut value=0.;
        for (interval,sweeps) in self.sweeps.iter().enumerate() {
            let step=interval+1;let sweep=sweeps.last().ok_or_else(||CaeError::contract("native split interval lacks a final sweep"))?;
            let full=k.expand(step,&self.states[step])?;let (data,_)=k.s.local_data(step);
            for e in 0..k.s.ne {
                let current=k.s.element_local(step,e,&full[k.solid_slice.clone()],&data);let x=k.s.element_design(e,&self.design);
                value+=sweep.map.local.heat_density_J_m3[e]*k.s.model.fields(&k.s.mesh.gradients[e],&current,&x).volume;
            }
        }
        if !value.is_finite() || value<0. {return Err(CaeError::convergence("invalid native split accumulated heat response"));}
        Ok(value)
    }
    pub fn integrated_heat_response_and_gradient(&self)->CaeResult<(f64,Vec<f64>)> {
        let (value,direct,seeds)=self.integrated_heat_partials()?;
        let gradient=self.pullback_with_heat(&vec![vec![0.;self.kernel.state_size];self.states.len()],&direct,&seeds)?;
        Ok((value,gradient))
    }
    pub fn integrated_heat_partials(&self)->CaeResult<(f64,Vec<f64>,Vec<Vec<f64>>)> {
        use implexity_ad::Dual;
        let k=&self.kernel;let mut value=0.;let mut direct=vec![0.;self.design.len()];let mut seeds=Vec::new();
        for (interval,sweeps) in self.sweeps.iter().enumerate() {
            let step=interval+1;let sweep=sweeps.last().ok_or_else(||CaeError::contract("native split interval lacks a final sweep"))?;
            let full=k.expand(step,&self.states[step])?;let (data,_)=k.s.local_data(step);let mut heat_seeds=Vec::with_capacity(k.s.ne);
            for e in 0..k.s.ne {
                let current=k.s.element_local(step,e,&full[k.solid_slice.clone()],&data);let x=k.s.element_design(e,&self.design);let volume=k.s.model.fields(&k.s.mesh.gradients[e],&current,&x).volume;
                let q=sweep.map.local.heat_density_J_m3[e];value+=q*volume;heat_seeds.push(volume);
                let current_ad:Vec<Dual<1>>=current.iter().map(|v|Dual::constant(*v)).collect();let owner=k.s.mesh.owners[e];let map=[owner,k.nc,k.nc+1,k.nc+2,k.nc+3+owner];
                for column in 0..5 {
                    let mut design_ad:Vec<Dual<1>>=x.iter().map(|v|Dual::constant(*v)).collect();design_ad[column]=Dual::variable(x[column],0);
                    let volume_ad=k.s.model.fields(&k.s.mesh.gradients[e],&current_ad,&design_ad).volume;
                    direct[map[column]]+=q*volume_ad.eps[0];
                }
            }
            seeds.push(heat_seeds);
        }
        if !value.is_finite() || value<0. {return Err(CaeError::convergence("invalid native split accumulated heat response"));}
        Ok((value,direct,seeds))
    }
}

pub fn native_split_cache_key(kernel:&UnifiedKernel,design_state_id:&str,runtime_source_sha256:&str,options:&super::split_history::SplitHistoryOptions,local_options:&implexity_physics_solid::history::local_advance::LocalAdvanceOptions)->CaeResult<String> {
    options.validate()?;
    if design_state_id.is_empty() || runtime_source_sha256.is_empty() || !local_options.tolerance.is_finite() || local_options.tolerance<=0. || local_options.maximum_iterations==0 || !local_options.condition_limit.is_finite() || local_options.condition_limit<=1. {return Err(CaeError::contract("native split cache requires bound design, source and numerical policy"));}
    let identity=serde_json::json!({"schema":"implexity-native-operator-split-history-cache/1","design_state_id":design_state_id,"runtime_source_sha256":runtime_source_sha256,"physical_problem_sha256":implexity_core::json::canonical_sha256(&kernel.p),"approximation":options.identity(),"local_numerics":{"tolerance":local_options.tolerance,"maximum_iterations":local_options.maximum_iterations,"condition_limit":local_options.condition_limit}});
    Ok(format!("declared-approximate-native-history-{}",implexity_core::json::sha256_hex(implexity_core::json::canonical(&identity).as_bytes())))
}

pub fn solve_native_age_condition(kernel:Arc<UnifiedKernel>,design:&[f64],options:&super::split_history::SplitHistoryOptions,context:Option<&implexity_solve::operation_context::OperationExecutionContext>)->CaeResult<NativeSplitTrajectory> {
 solve_native_age_condition_with_reference_guess(kernel,design,options,context,None)
}

pub fn solve_native_age_condition_with_reference_guess(kernel:Arc<UnifiedKernel>,design:&[f64],options:&super::split_history::SplitHistoryOptions,context:Option<&implexity_solve::operation_context::OperationExecutionContext>,reference_guess:Option<&[f64]>)->CaeResult<NativeSplitTrajectory> {
 use implexity_physics_solid::history::local_snapshot::native_age_condition_snapshot;
 options.validate()?;
 if !options.aged_condition || kernel.nt!=2 || options.snapshot_times_s!=kernel.s.times {return Err(CaeError::contract("native aged condition requires one declared age interval"));}
 let operation=kernel.canonical_context(context,"declared_frozen_reference_aged_condition")?;let initial=kernel.reduction.initial_for(design)?;let old_full=kernel.expand(0,&initial)?;let imported=kernel.take_numerical_initial_guess(design)?;
 if reference_guess.is_some_and(|state|state.len()!=kernel.state_size || state.iter().any(|v|!v.is_finite())) {return Err(CaeError::contract("reference numerical guess has incompatible native layout"));}
 let mut driving=imported.map_or_else(||reference_guess.map_or_else(||initial.clone(),<[f64]>::to_vec),|states|states[1].clone());let mut stages=Vec::new();
 for enabled in [false,true] {let full=kernel.expand(1,&driving)?;let local=Arc::new(if options.stress_relaxing_condition {implexity_physics_solid::history::local_snapshot::native_prescribed_relaxed_age_snapshot(&kernel.s,1,&full[kernel.solid_slice.clone()],&old_full[kernel.solid_slice.clone()],design,enabled)?}else{native_age_condition_snapshot(&kernel.s,1,&full[kernel.solid_slice.clone()],&old_full[kernel.solid_slice.clone()],design,enabled)?});let mut map=NativeSplitSnapshotMap::new(Arc::clone(&kernel),1,initial.clone(),local)?;map.operation=Some(operation.clone());implexity_solve::trace::point("aged_condition.equilibrium_started",||{let mut fields=operation.trace_fields();fields.insert("age_s".into(),serde_json::json!(kernel.s.times[1]));fields.insert("stage".into(),serde_json::json!(if enabled {"aged_condition"}else{"unaged_reference"}));fields})?;let solved=map.solve(&driving,design)?;driving=solved.state.clone();stages.push(NativeSplitSweep{map,solved});}
 Ok(NativeSplitTrajectory{kernel:Arc::clone(&kernel),design:design.to_vec(),states:vec![initial,driving],sweeps:vec![stages],identity:serde_json::json!({"approximation":options.identity(),"physical_problem_sha256":implexity_core::json::canonical_sha256(&kernel.p),"global_snapshot_solves":2,"initialization_auxiliary_solves":0,"local_newton_solves":0,"local_algebraic_age_maps":kernel.s.ne})})
}
