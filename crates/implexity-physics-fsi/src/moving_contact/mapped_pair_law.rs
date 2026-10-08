// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use crate::moving_contact::{complementarity::{self, OriginBranch}, contact_field::{ContactContribution, NativeContactLaw}, mapped_contact::MappedContact, surface_map::SurfaceFeature};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::{matrix::Jacobian, time_stepper::StepParameters};
use std::collections::BTreeSet;
fn fail(s: &str) -> CaeError { CaeError::contract(s) }
#[derive(Clone)]
pub struct NodeBinding {
    pub reference_m: [f64; 3],
    pub state: [usize; 3],
    pub inverse_state_scale: [f64; 3],
    pub force_flux: [usize; 3],
}
pub struct MappedPairLaw {
    nodes: [Vec<NodeBinding>; 2],
    features: [SurfaceFeature; 4],
    bodies: [usize; 4],
    phase: CsrMatrix,
    native_states: usize,
    fluxes: usize,
    gap_scale: f64,
    force_scale: f64,
    path_policy: crate::moving_contact::linear_path::PathPolicy,
    state_columns: Vec<usize>,
    design_columns: Vec<usize>,
}
impl MappedPairLaw {
    pub fn fixed_reference_geometry(&self) -> bool { self.features.iter().all(|f|matches!(f,SurfaceFeature::Vertex(_)|SurfaceFeature::FixedTetrahedron{..})) }
    pub fn new(nodes: [Vec<NodeBinding>; 2], features: [SurfaceFeature; 4], bodies: [usize; 4], phase: CsrMatrix, native_states: usize, fluxes: usize, gap_scale: f64, force_scale: f64) -> CaeResult<Self> {
        let count = nodes[0].len().checked_add(nodes[1].len()).ok_or_else(|| fail("phase count overflow"))?;
        if native_states == 0 || native_states == usize::MAX || fluxes == 0 || phase.nrows() != count
            || bodies.iter().any(|b| *b > 1) || bodies[0] == bodies[1]
            || bodies[1] != bodies[2] || bodies[2] != bodies[3]
            || ![gap_scale, force_scale].iter().all(|v| v.is_finite() && *v > 0.) {
            return Err(fail("invalid mapped contact layout"));
        }
        let mut states = BTreeSet::new(); let mut forces = BTreeSet::new();
        for node in nodes.iter().flatten() {
            if node.reference_m.iter().any(|v| !v.is_finite())
                || node.inverse_state_scale.iter().any(|v| !v.is_finite() || *v <= 0.) {
                return Err(fail("invalid physical node binding"));
            }
            for i in 0..3 {
                if node.state[i] >= native_states || node.force_flux[i] >= fluxes
                    || !states.insert(node.state[i]) || !forces.insert(node.force_flux[i]) {
                    return Err(fail("conflicting mapped contact indices"));
                }
            }
        }
        let mut sc = BTreeSet::new(); let mut dc = BTreeSet::new();
        for k in 0..4 {
            let b = bodies[k]; let offset = if b == 0 { 0 } else { nodes[0].len() };
            let selected: Vec<_> = match features[k] {
                SurfaceFeature::Vertex(i) => vec![i],
                SurfaceFeature::FixedTetrahedron{nodes,..}=>nodes.to_vec(),
                SurfaceFeature::DensityEdge {nodes: [i,j], ..} => vec![i,j],
            };
            for i in selected {
                let node = nodes[b].get(i).ok_or_else(|| fail("mapped feature index"))?;
                sc.extend(node.state);
                let (js, vs) = phase.row(offset + i);
                if vs.iter().any(|v| !v.is_finite()) { return Err(fail("phase map nonfinite")); }
                dc.extend(js.iter().copied());
            }
        }
        Ok(Self { nodes, features, bodies, phase, native_states, fluxes, gap_scale, force_scale,
            path_policy: crate::moving_contact::linear_path::PathPolicy::default(), state_columns: sc.into_iter().collect(), design_columns: dc.into_iter().collect() })
    }
    pub fn with_residual_path_allowance(self,residual_tolerance:f64)->CaeResult<Self>{
        let allowance=crate::moving_contact::linear_path::ResidualPathAllowance::new(residual_tolerance,self.gap_scale).map_err(fail)?;
        let policy=allowance.policy(self.path_policy).map_err(fail)?;self.with_path_policy(policy)
    }
    pub fn residual_path_certificate(&self,previous:&[f64],current:&[f64],design:&[f64],residual_tolerance:f64)->CaeResult<crate::moving_contact::linear_path::ApproximatePathCertificate>{
        let allowance=crate::moving_contact::linear_path::ResidualPathAllowance::new(residual_tolerance,self.gap_scale).map_err(fail)?;
        if self.path_policy.minimum_signed_gap.to_bits()!=(-allowance.allowance_m).to_bits(){return Err(fail("contact path allowance identity"));}
        if current.len()!=self.native_states+1||previous.len()!=self.native_states+1{return Err(fail("residual path complete state shape"));}
        if !current[self.native_states].is_finite()||current[self.native_states]<0.{return Err(fail("residual path nonnegative finite multiplier"));}
        let density=self.phases(design)?;let mut strict=self.path_policy;strict.minimum_signed_gap=0.;
        crate::moving_contact::linear_path::certify_residual_path(self.points(previous,&density)?,self.points(current,&density)?,strict,allowance).map_err(fail)
    }
    pub fn with_path_policy(mut self, policy: crate::moving_contact::linear_path::PathPolicy) -> CaeResult<Self> {
        if !(policy.minimum_barycentric > 0. && policy.minimum_barycentric < 1./3.)
            || !(policy.minimum_area_ratio > 0. && policy.minimum_area_ratio < 1.)
            || !policy.minimum_signed_gap.is_finite()
            || !(policy.time_resolution > 0. && policy.time_resolution < 1.)
            || policy.maximum_intervals == 0 || policy.maximum_depth == 0 || policy.maximum_depth > 64 {
            return Err(fail("invalid contact path policy"));
        }
        self.path_policy=policy; Ok(self)
    }
    fn points(&self, state: &[f64], density: &[Vec<f64>;2]) -> CaeResult<[[f64;3];4]> {
        let q=self.coordinates(state)?; let mut points=[[0.;3];4];
        for k in 0..4 {let b=self.bodies[k];points[k]=self.features[k].map(&density[b])?.position(&q[b])?;}
        Ok(points)
    }
    fn coordinates(&self, state: &[f64]) -> CaeResult<[Vec<[f64;3]>;2]> {
        if state.len() != self.native_states + 1 || state.iter().any(|v| !v.is_finite()) {
            return Err(fail("mapped complete state shape"));
        }
        let out: [Vec<[f64;3]>;2] = std::array::from_fn(|b| self.nodes[b].iter().map(|n| std::array::from_fn(|i| n.reference_m[i] + n.inverse_state_scale[i] * state[n.state[i]])).collect());
        if out.iter().flatten().flatten().any(|v| !v.is_finite()) { return Err(fail("mapped coordinate overflow")); }
        Ok(out)
    }
    fn phases(&self, design: &[f64]) -> CaeResult<[Vec<f64>;2]> {
        if design.len() != self.phase.ncols() || design.iter().any(|v| !v.is_finite()) { return Err(fail("mapped phase design shape")); }
        let mut values = Vec::with_capacity(self.phase.nrows());
        for r in 0..self.phase.nrows() {
            let (js, vs) = self.phase.row(r);
            let v: f64 = js.iter().zip(vs).map(|(&j,&a)| a * design[j]).sum();
            if !v.is_finite() || !(0. ..=1.).contains(&v) { return Err(fail("mapped phase domain")); }
            values.push(v);
        }
        Ok([values[..self.nodes[0].len()].to_vec(), values[self.nodes[0].len()..].to_vec()])
    }
    fn zeros(&self) -> [Vec<[f64;3]>;2] { std::array::from_fn(|b| vec![[0.;3];self.nodes[b].len()]) }
    fn state_direction(&self, column: usize) -> [Vec<[f64;3]>;2] {
        std::array::from_fn(|b| self.nodes[b].iter().map(|n| std::array::from_fn(|i| if n.state[i] == column { n.inverse_state_scale[i] } else { 0. })).collect())
    }
    fn phase_direction(&self, column: usize) -> [Vec<f64>;2] {
        std::array::from_fn(|b| self.nodes[b].iter().enumerate().map(|(i,_)| {
            let r = i + if b == 0 {0} else {self.nodes[0].len()};
            let (js,vs) = self.phase.row(r);
            js.iter().zip(vs).filter(|(j,_)| **j == column).map(|(_,v)| *v).sum()
        }).collect())
    }
    fn flatten_force(&self, f: &[Vec<[f64;3]>;2]) -> CaeResult<Vec<f64>> {
        let mut out = vec![0.;self.fluxes];
        for b in 0..2 { for (node,v) in self.nodes[b].iter().zip(&f[b]) { for i in 0..3 {out[node.force_flux[i]] += v[i];} } }
        if out.iter().any(|v| !v.is_finite()) { return Err(fail("mapped force overflow")); }
        Ok(out)
    }
    pub fn instantaneous_row(&self, current:&[f64], design:&[f64]) -> CaeResult<(Vec<f64>,f64)> {
        let q=self.coordinates(current)?;let density=self.phases(design)?;
        let map=MappedContact::evaluate(&self.features,self.bodies,&q,&q,&density)?;
        let zero=self.zeros();let dr=std::array::from_fn(|b|vec![0.;self.nodes[b].len()]);
        let force=map.force_direction(1.,0.,&zero,&zero,&dr)?;
        Ok((self.flatten_force(&force.force)?,map.geometry().gap1))
    }
    pub fn gap_current_matrix(&self,current:&[f64],design:&[f64])->CaeResult<(f64,Jacobian)>{
        let(row,gap)=self.instantaneous_row(current,design)?;let mut entries=vec![];
        for node in self.nodes.iter().flatten(){for a in 0..3{let v=row[node.force_flux[a]]*node.inverse_state_scale[a]/self.gap_scale;if v!=0.{entries.push((0,node.state[a],v));}}}
        Ok((gap/self.gap_scale,matrix(1,self.native_states+1,&entries)?))
    }
    pub fn instantaneous_row_direction(&self,current:&[f64],design:&[f64],dcurrent:&[f64],ddesign:&[f64])->CaeResult<(Vec<f64>,f64)> {
        if dcurrent.len()!=self.native_states+1 || ddesign.len()!=self.phase.ncols() || dcurrent.iter().chain(ddesign).any(|v|!v.is_finite()) {return Err(fail("instantaneous contact direction shape"));}
        let q=self.coordinates(current)?;let density=self.phases(design)?;
        let map=MappedContact::evaluate(&self.features,self.bodies,&q,&q,&density)?;
        let dq=std::array::from_fn(|b|self.nodes[b].iter().map(|node|std::array::from_fn(|a|node.inverse_state_scale[a]*dcurrent[node.state[a]])).collect());
        let mut dr: [Vec<f64>;2]=std::array::from_fn(|b|vec![0.;self.nodes[b].len()]);
        for (j,&v) in ddesign.iter().enumerate(){if v!=0. {let d=self.phase_direction(j);for b in 0..2{for i in 0..dr[b].len(){dr[b][i]+=v*d[b][i];}}}}
        let force=map.force_direction(1.,0.,&dq,&dq,&dr)?;
        Ok((self.flatten_force(&force.direction)?,force.gap_direction))
    }
    pub fn instantaneous_row_adjoint(&self,current:&[f64],design:&[f64],row_bar:&[f64],gap_bar:f64)->CaeResult<(Vec<f64>,Vec<f64>)>{
        if row_bar.len()!=self.fluxes || row_bar.iter().any(|v|!v.is_finite()) || !gap_bar.is_finite(){return Err(fail("instantaneous contact cotangent shape"));}
        let mut state=vec![0.;self.native_states+1];let mut params=vec![0.;self.phase.ncols()];
        let mut ds=vec![0.;state.len()];let mut dp=vec![0.;params.len()];
        for &j in &self.state_columns{ds[j]=1.;let(r,g)=self.instantaneous_row_direction(current,design,&ds,&dp)?;state[j]=r.iter().zip(row_bar).map(|(a,b)|a*b).sum::<f64>()+g*gap_bar;ds[j]=0.;}
        for &j in &self.design_columns{dp[j]=1.;let(r,g)=self.instantaneous_row_direction(current,design,&ds,&dp)?;params[j]=r.iter().zip(row_bar).map(|(a,b)|a*b).sum::<f64>()+g*gap_bar;dp[j]=0.;}
        if state.iter().chain(&params).any(|x|!x.is_finite()){return Err(fail("instantaneous contact adjoint overflow"));}Ok((state,params))
    }
    pub fn gap_design_matrix(&self,current:&[f64],design:&[f64])->CaeResult<Jacobian>{
        let ds=vec![0.;self.native_states+1];let mut dp=vec![0.;self.phase.ncols()];let mut entries=vec![];
        for &j in &self.design_columns{dp[j]=1.;let(_,g)=self.instantaneous_row_direction(current,design,&ds,&dp)?;if g!=0.{entries.push((0,j,g/self.gap_scale));}dp[j]=0.;}
        matrix(1,self.phase.ncols(),&entries)
    }
    pub fn strict_branch(&self, current: &[f64], previous: &[f64], design: &[f64]) -> CaeResult<()> {
        let old = self.coordinates(previous)?; let new = self.coordinates(current)?; let rho = self.phases(design)?;
        let map = MappedContact::evaluate(&self.features,self.bodies,&old,&new,&rho)?;
        let g = map.geometry().gap1; let l = current[self.native_states];
        if (g == 0. && l > 0.) || (g > 0. && l == 0.) { Ok(()) }
        else { Err(fail("contact ordinary branch not strict")) }
    }
}
fn matrix(rows: usize, cols: usize, entries: &[(usize,usize,f64)]) -> CaeResult<Jacobian> {
    if entries.iter().any(|e| !e.2.is_finite()) { return Err(fail("mapped derivative overflow")); }
    let r: Vec<_> = entries.iter().map(|e| e.0).collect(); let c: Vec<_> = entries.iter().map(|e| e.1).collect(); let v: Vec<_> = entries.iter().map(|e| e.2).collect();
    Ok(Jacobian::Csr(CsrMatrix::from_triplets(rows,cols,&r,&c,&v).map_err(|e| fail(&e.to_string()))?))
}
impl NativeContactLaw for MappedPairLaw {
    fn check_derivative_domain(&self, _n: usize, current: &[f64], previous: &[f64], p: StepParameters<'_>) -> CaeResult<()> {
        let old=self.coordinates(previous)?;let new=self.coordinates(current)?;let density=self.phases(p.design)?;
        let map=MappedContact::evaluate(&self.features,self.bodies,&old,&new,&density)?;
        let gap=map.geometry().gap1/self.gap_scale;
        let multiplier=current[self.native_states]/self.force_scale;
        if !gap.is_finite() || !multiplier.is_finite() {
            return Err(fail("contact derivative normalization overflow"));
        }
        if gap==0. && multiplier==0. {
            return Err(fail("biactive contact origin has no ordinary derivative"));
        }
        Ok(())
    }
    fn check_state_domain(&self, _n: usize, current: &[f64], previous: &[f64], p: StepParameters<'_>) -> CaeResult<()> {
        let density=self.phases(p.design)?;
        let report=crate::moving_contact::linear_path::check_linear_path(self.points(previous,&density)?,self.points(current,&density)?,self.path_policy).map_err(fail)?;
        if report.status != crate::moving_contact::linear_path::PathStatus::Admitted {return Err(fail("complete contact path not admitted"));}
        if current[self.native_states] < 0. {return Err(fail("negative converged contact multiplier"));}
        Ok(())
    }
    fn newton_constraints(&self, _n: usize, current: &[f64], previous: &[f64], p: StepParameters<'_>, direction: &[f64], attempt: usize) -> CaeResult<Option<Jacobian>> {
        let old=self.coordinates(previous)?;let new=self.coordinates(current)?;let density=self.phases(p.design)?;
        let map=MappedContact::evaluate(&self.features,self.bodies,&old,&new,&density)?;
        if map.geometry().gap1!=0. || current[self.native_states]!=0. {return Ok(None)}
        if direction.len()!=self.native_states+1 || direction.iter().any(|v| !v.is_finite()) {return Err(fail("invalid origin direction"));}
        if attempt==0 && direction[self.native_states]>=0. {return Ok(None)}
        if attempt==0 {return Ok(Some(matrix(1,self.native_states+1,&[(0,self.native_states,-1./self.force_scale)])?))}
        if attempt!=1 {return Err(fail("contact origin alternatives exhausted"));}
        let zero=self.zeros();let dq=std::array::from_fn(|b|self.nodes[b].iter().map(|node|std::array::from_fn(|i|node.inverse_state_scale[i]*direction[node.state[i]])).collect());
        let dr=std::array::from_fn(|b|vec![0.;self.nodes[b].len()]);
        let a=map.force_direction(0.,0.,&zero,&dq,&dr)?;
        if a.gap_direction>=0. {Ok(None)}else{Err(fail("no cone-consistent contact origin direction"))}
    }

    fn multipliers(&self) -> usize {1}
    fn evaluate(&self, _n: usize, current: &[f64], previous: &[f64], p: StepParameters<'_>) -> CaeResult<ContactContribution> {
        let old = self.coordinates(previous)?; let new = self.coordinates(current)?; let rho = self.phases(p.design)?;
        let map = MappedContact::evaluate(&self.features,self.bodies,&old,&new,&rho)?;
        let zero = self.zeros(); let zr: [Vec<f64>;2] = std::array::from_fn(|b| vec![0.;self.nodes[b].len()]);
        let lambda = current[self.native_states];
        let (phi,pg,pl) = complementarity::residual_partials(map.geometry().gap1,lambda,self.gap_scale,self.force_scale,OriginBranch::Closed)?;
        let baseline = map.force_direction(lambda,0.,&zero,&zero,&zr)?;
        let mut fc=vec![]; let mut fp=vec![]; let mut fd=vec![]; let mut gc=vec![]; let mut gd=vec![];
        for &j in self.state_columns.iter().chain(std::iter::once(&self.native_states)) {
            let d = self.state_direction(j);
            let a = map.force_direction(lambda, if j == self.native_states {1.} else {0.}, &zero,&d,&zr)?;
            for (i,v) in self.flatten_force(&a.direction)?.into_iter().enumerate() { if v != 0. {fc.push((i,j,v));} }
            let v = pg * a.gap_direction + if j == self.native_states {pl} else {0.};
            if v != 0. {gc.push((0,j,v));}
            if j != self.native_states {
                let a = map.force_direction(lambda,0.,&d,&zero,&zr)?;
                for (i,v) in self.flatten_force(&a.direction)?.into_iter().enumerate() { if v != 0. {fp.push((i,j,v));} }
            }
        }
        for &j in &self.design_columns {
            let dr = self.phase_direction(j);
            let a = map.force_direction(lambda,0.,&zero,&zero,&dr)?;
            for (i,v) in self.flatten_force(&a.direction)?.into_iter().enumerate() { if v != 0. {fd.push((i,j,v));} }
            let v = pg * a.gap_direction; if v != 0. {gd.push((0,j,v));}
        }
        let total=self.native_states+1; let nd=self.phase.ncols();
        Ok(ContactContribution {force:self.flatten_force(&baseline.force)?,constraints:vec![phi],
            force_current:matrix(self.fluxes,total,&fc)?, force_previous:matrix(self.fluxes,total,&fp)?,force_design:matrix(self.fluxes,nd,&fd)?,force_time:vec![0.;self.fluxes],
            constraint_current:matrix(1,total,&gc)?,constraint_previous:matrix(1,total,&[])?,constraint_design:matrix(1,nd,&gd)?,constraint_time:vec![0.]})
    }
    fn admitted_trial(&self, _n: usize, current: &[f64], direction: &[f64], previous: &[f64], p: StepParameters<'_>, trial: f64) -> CaeResult<f64> {
        if direction.len()!=self.native_states+1 || direction.iter().any(|v| !v.is_finite()) || !trial.is_finite() || trial<=0. || trial>1. {return Err(fail("invalid contact trial"));}
        let density=self.phases(p.design)?;let old=self.points(previous,&density)?;
        let mut fraction=trial;
        for _ in 0..24 {
            let candidate:Vec<_>=current.iter().zip(direction).map(|(x,d)|x+fraction*d).collect();
            let report=crate::moving_contact::linear_path::check_linear_path(old,self.points(&candidate,&density)?,self.path_policy).map_err(fail)?;

            if report.status==crate::moving_contact::linear_path::PathStatus::Admitted {return Ok(fraction)}
            fraction*=0.5;
        }
        let candidate:Vec<_>=current.iter().zip(direction).map(|(x,d)|x+fraction*d).collect();
        let report=crate::moving_contact::linear_path::check_linear_path(old,self.points(&candidate,&density)?,self.path_policy).map_err(fail)?;
        Err(fail(&format!("contact Newton path fraction unresolved; previous_gap={}; current_gap={}; candidate_gap={}; fraction={fraction}; report={report:?}",self.instantaneous_row(previous,p.design)?.1,self.instantaneous_row(current,p.design)?.1,self.instantaneous_row(&candidate,p.design)?.1)))
    }
}
