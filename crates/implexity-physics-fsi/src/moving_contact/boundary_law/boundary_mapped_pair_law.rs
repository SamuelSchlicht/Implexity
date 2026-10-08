// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use crate::moving_contact::{complementarity::{self,OriginBranch},contact_field::{ContactContribution,NativeContactLaw},surface_map::SurfaceFeature};
use super::boundary_mapped_contact::MappedContact;
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
pub struct BoundaryMappedPairLaw {
    kind: super::boundary_discrete_geometry::SelectedKind,
    nodes: [Vec<NodeBinding>; 2],
    features: [SurfaceFeature; 4],
    bodies: [usize; 4],
    phase: CsrMatrix,
    native_states: usize,
    fluxes: usize,
    gap_scale: f64,
    force_scale: f64,
    path_policy: super::boundary_path::PathPolicy,
    state_columns: Vec<usize>,
    design_columns: Vec<usize>,
    source_star: Vec<(SurfaceFeature,[f64;3])>,
    target_star: Vec<(SurfaceFeature,[f64;3])>,
    target_vertex_slot: usize,
    geometry_identity: String,
    references:[[f64;3];4],
    density_surfaces:Option<[super::exposed_surface::ExposedSurface;2]>,
}
impl BoundaryMappedPairLaw {
    pub fn fixed_reference_geometry(&self)->bool{self.density_surfaces.is_none()}
    fn new(nodes: [Vec<NodeBinding>; 2], features: [SurfaceFeature; 4], bodies: [usize; 4], phase: CsrMatrix, native_states: usize, fluxes: usize, gap_scale: f64, force_scale: f64) -> CaeResult<Self> {
        Self::new_selected(super::boundary_discrete_geometry::SelectedKind::VertexFace,nodes,features,bodies,phase,native_states,fluxes,gap_scale,force_scale)
    }
    fn new_selected(kind:super::boundary_discrete_geometry::SelectedKind,nodes: [Vec<NodeBinding>; 2], features: [SurfaceFeature; 4], bodies: [usize; 4], phase: CsrMatrix, native_states: usize, fluxes: usize, gap_scale: f64, force_scale: f64) -> CaeResult<Self> {
        let count = nodes[0].len().checked_add(nodes[1].len()).ok_or_else(|| fail("phase count overflow"))?;
        if native_states == 0 || native_states == usize::MAX || fluxes == 0 || phase.nrows() != count
            || bodies.iter().any(|b| *b > 1) || !match kind{super::boundary_discrete_geometry::SelectedKind::VertexFace=>bodies[0]!=bodies[1]&&bodies[1]==bodies[2]&&bodies[2]==bodies[3],super::boundary_discrete_geometry::SelectedKind::EdgeEdge{orientation}=>bodies[0]==bodies[1]&&bodies[2]==bodies[3]&&bodies[0]!=bodies[2]&&(orientation==1.||orientation== -1.)}
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
        Ok(Self { kind, nodes, features, bodies, phase, native_states, fluxes, gap_scale, force_scale,
            path_policy: super::boundary_path::PathPolicy::default(), state_columns: sc.into_iter().collect(), design_columns: dc.into_iter().collect(),source_star:vec![],target_star:vec![],target_vertex_slot:0,geometry_identity:String::new(),references:[[0.;3];4],density_surfaces:None })
    }
    pub fn from_authenticated_patches(nodes:[Vec<NodeBinding>;2],phase:CsrMatrix,native_states:usize,fluxes:usize,gap_scale:f64,force_scale:f64,source:&super::reference_embedding::AuthenticatedReferencePatch,target:&super::reference_embedding::AuthenticatedReferencePatch,source_vertex:usize,target_vertex:usize,target_facet:usize)->CaeResult<Self>{
        let sp=source.patch();let tp=target.patch();if sp.body()>1||tp.body()>1||sp.body()==tp.body(){return Err(fail("boundary reference body identity"));}
        for patch in [source,target]{let b=patch.patch().body();if nodes[b].len()!=patch.nodes().len()||nodes[b].iter().zip(patch.nodes()).any(|(n,p)|n.reference_m.map(f64::to_bits)!=p.map(f64::to_bits)){return Err(fail("boundary native grid/reference binding"));}}
        let facet=*tp.facets().get(target_facet).ok_or_else(||fail("boundary surface facet"))?;let slot=facet.iter().position(|v|*v==target_vertex).ok_or_else(||fail("boundary facet owns vertex"))?;
        let source_feature=sp.features().get(source_vertex).ok_or_else(||fail("boundary source vertex"))?.clone();let features=[source_feature,tp.features()[facet[0]].clone(),tp.features()[facet[1]].clone(),tp.features()[facet[2]].clone()];let bodies=[sp.body(),tp.body(),tp.body(),tp.body()];let mut out=Self::new(nodes,features,bodies,phase,native_states,fluxes,gap_scale,force_scale)?;
        let star=|patch:&super::reference_embedding::AuthenticatedReferencePatch,vertex:usize|->CaeResult<Vec<(SurfaceFeature,[f64;3])>>{let p=patch.patch();let mut ids=BTreeSet::new();for f in p.incident_facets(vertex)?{ids.extend(p.facets()[*f]);}ids.into_iter().map(|i|Ok((p.features()[i].clone(),patch.reference(i)?))).collect()};out.source_star=star(source,source_vertex)?;out.target_star=star(target,target_vertex)?;out.target_vertex_slot=slot;out.references=[source.reference(source_vertex)?,target.reference(facet[0])?,target.reference(facet[1])?,target.reference(facet[2])?];out.geometry_identity=implexity_core::json::sha256_hex(format!("{}:{}:{source_vertex}:{target_vertex}:{target_facet}",source.identity(),target.identity()).as_bytes());Ok(out)
    }
    pub fn from_authenticated_edge_pair(nodes:[Vec<NodeBinding>;2],phase:CsrMatrix,native_states:usize,fluxes:usize,gap_scale:f64,force_scale:f64,patches:[&super::reference_embedding::AuthenticatedReferencePatch;2],edges:[[usize;2];2],orientation:f64)->CaeResult<Self>{
        for b in 0..2{let patch=patches[b];if patch.patch().body()!=b||nodes[b].len()!=patch.nodes().len()||nodes[b].iter().zip(patch.nodes()).any(|(n,p)|n.reference_m.map(f64::to_bits)!=p.map(f64::to_bits))||edges[b][0]==edges[b][1]||!patch.patch().facets().iter().any(|f|edges[b].iter().all(|i|f.contains(i))){return Err(fail("authenticated native boundary edge ownership"));}}
        let features=std::array::from_fn(|k|patches[k/2].patch().features()[edges[k/2][k%2]].clone());let mut out=Self::new_selected(super::boundary_discrete_geometry::SelectedKind::EdgeEdge{orientation},nodes,features,[0,0,1,1],phase,native_states,fluxes,gap_scale,force_scale)?;
        out.references=[patches[0].reference(edges[0][0])?,patches[0].reference(edges[0][1])?,patches[1].reference(edges[1][0])?,patches[1].reference(edges[1][1])?];let star=|b:usize|->CaeResult<Vec<(SurfaceFeature,[f64;3])>>{let patch=patches[b];let mut ids=BTreeSet::new();for vertex in edges[b]{for f in patch.patch().incident_facets(vertex)?{ids.extend(patch.patch().facets()[*f]);}}ids.into_iter().map(|i|Ok((patch.patch().features()[i].clone(),patch.reference(i)?))).collect()};out.source_star=star(0)?;out.target_star=star(1)?;out.geometry_identity=implexity_core::json::canonical_sha256(&serde_json::json!({"patches":[patches[0].identity(),patches[1].identity()],"edges":edges,"orientation":orientation,"kind":"native nonparallel edge-edge"}));Ok(out)
    }
    pub fn from_density_surface_vertex_pair(nodes:[Vec<NodeBinding>;2],phase:CsrMatrix,native_states:usize,fluxes:usize,gap_scale:f64,force_scale:f64,surfaces:[&super::surface_admission::DensitySurfaceMap;2],design:&[f64],source_body:usize,source_vertex:usize,target_vertex:usize,target_facet:usize)->CaeResult<Self>{
        if source_body>1{return Err(fail("density surface source body"));}let target_body=1-source_body;
        for b in 0..2{if surfaces[b].body()!=b||nodes[b].len()!=surfaces[b].reference_nodes().len()||nodes[b].iter().zip(surfaces[b].reference_nodes()).any(|(n,p)|n.reference_m.map(f64::to_bits)!=p.map(f64::to_bits)){return Err(fail("density surface native reference binding"));}}
        let source=surfaces[source_body].surface();let target=surfaces[target_body].surface();let facet=*target.facets().get(target_facet).ok_or_else(||fail("density surface target facet"))?;let slot=facet.iter().position(|v|*v==target_vertex).ok_or_else(||fail("density surface target vertex ownership"))?;
        let sf=source.features().get(source_vertex).ok_or_else(||fail("density surface source vertex"))?.clone();let features=[sf,target.features()[facet[0]].clone(),target.features()[facet[1]].clone(),target.features()[facet[2]].clone()];let mut out=Self::new(nodes,features,[source_body,target_body,target_body,target_body],phase,native_states,fluxes,gap_scale,force_scale)?;
        out.density_surfaces=Some([surfaces[0].surface().clone(),surfaces[1].surface().clone()]);let rho=out.phases(design)?;
        for b in 0..2{surfaces[b].surface().require_fixed_stratum(&rho[b])?;}
        let star=|surface:&super::exposed_surface::ExposedSurface,vertex:usize|->CaeResult<Vec<(SurfaceFeature,[f64;3])>>{if vertex>=surface.features().len(){return Err(fail("density surface star vertex"));}let mut ids=BTreeSet::new();for facet in surface.facets(){if facet.contains(&vertex){ids.extend(*facet);}}if ids.is_empty(){return Err(fail("density surface empty star"));}Ok(ids.into_iter().map(|i|(surface.features()[i].clone(),[0.;3])).collect())};
        out.source_star=star(source,source_vertex)?;out.target_star=star(target,target_vertex)?;out.target_vertex_slot=slot;
        out.geometry_identity=implexity_core::json::canonical_sha256(&serde_json::json!({"surface_strata":[source.stratum_identity(),target.stratum_identity()],"surface_meshes":[source.mesh_identity(),target.mesh_identity()],"source_body":source_body,"source_vertex":source_vertex,"target_vertex":target_vertex,"target_facet":target_facet}));Ok(out)
    }
    pub fn selected_path_report(&self,previous:&[f64],current:&[f64],design:&[f64])->CaeResult<super::boundary_path::PathReport>{if matches!(self.kind,super::boundary_discrete_geometry::SelectedKind::EdgeEdge{..}){return Err(fail("EE path uses its typed native parameter certificate"));}let density=self.phases(design)?;super::boundary_path::check_linear_path(self.points(previous,&density)?,self.points(current,&density)?,self.path_policy).map_err(fail)}
    pub fn oriented_normal_cone_certificate(&self,current:&[f64],design:&[f64])->CaeResult<super::normal_cone::OrientedNormalConeCertificate>{
        if self.geometry_identity.is_empty()||self.source_star.is_empty()||self.target_star.is_empty(){return Err(fail("authenticated complete incident stars required"));}
        let density=self.phases(design)?;let coordinates=self.coordinates(current)?;let points=self.points(current,&density)?;
        let sub=|a:[f64;3],b:[f64;3]|std::array::from_fn(|i|a[i]-b[i]);let cross=|a:[f64;3],b:[f64;3]|[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]];
        let(axis,source_body,target_body,target_anchor)=match self.kind{super::boundary_discrete_geometry::SelectedKind::VertexFace=>(cross(sub(points[2],points[1]),sub(points[3],points[1])),self.bodies[0],self.bodies[1],1+self.target_vertex_slot),super::boundary_discrete_geometry::SelectedKind::EdgeEdge{orientation}=>(cross(sub(points[1],points[0]),sub(points[3],points[2])).map(|v|orientation*v),self.bodies[0],self.bodies[2],2)};
        let mut rows=Vec::new();for(body,star,anchor,positive)in[(source_body,&self.source_star,0,true),(target_body,&self.target_star,target_anchor,false)]{for(feature,reference)in star{let map=feature.map(&density[body])?;let p=super::reference_embedding::position(&map,*reference,&coordinates[body])?;rows.push(sub(p,points[anchor]).map(|v|if positive{v}else{-v}));}}
        super::normal_cone::OrientedNormalConeCertificate::from_native_star(&rows,axis,&self.geometry_identity,serde_json::json!({"points":points,"physical_design":design,"state":current,"edge_edge":matches!(self.kind,super::boundary_discrete_geometry::SelectedKind::EdgeEdge{..})}))
    }
    pub fn geometry_identity(&self)->&str{&self.geometry_identity}
    pub fn require_topology_surface_reextraction_owner(&self)->CaeResult<()>{Err(fail("fixed reference polyhedral surface is not a topology-controlled re-extraction/shape-gradient owner"))}
    fn supporting_star_path(&self,previous:&[f64],current:&[f64],density:&[Vec<f64>;2])->CaeResult<()>{let a=self.coordinates(previous)?;let b=self.coordinates(current)?;let old=self.points(previous,density)?;let new=self.points(current,density)?;if let super::boundary_discrete_geometry::SelectedKind::EdgeEdge{orientation}=self.kind{super::edge_path::certify_edge_path(old,new,orientation,self.path_policy.maximum_intervals,self.path_policy.maximum_depth)?;for(body,star,anchor,positive)in[(self.bodies[0],&self.source_star,0,true),(self.bodies[2],&self.target_star,2,false)]{for(feature,reference)in star{let map=feature.map(&density[body])?;if !super::boundary_path::certify_supporting_edge_star_path(old,new,old[anchor],new[anchor],super::reference_embedding::position(&map,*reference,&a[body])?,super::reference_embedding::position(&map,*reference,&b[body])?,orientation,positive).map_err(fail)?{return Err(fail("selected edge normal cone whole path not certified"));}}}return Ok(());}let triangle_old=[old[1],old[2],old[3]];let triangle_new=[new[1],new[2],new[3]];for (body,star,anchor,positive) in [(self.bodies[0],&self.source_star,0,true),(self.bodies[1],&self.target_star,1+self.target_vertex_slot,false)]{for (feature,reference) in star{let map=feature.map(&density[body])?;let admitted=super::boundary_path::certify_supporting_star_path(triangle_old,triangle_new,old[anchor],new[anchor],super::reference_embedding::position(&map,*reference,&a[body])?,super::reference_embedding::position(&map,*reference,&b[body])?,positive).map_err(fail)?;if !admitted{return Err(fail("selected surface supporting-normal cone path not certified"));}}}Ok(())}
    pub fn with_residual_path_allowance(self,residual_tolerance:f64)->CaeResult<Self>{
        if matches!(self.kind,super::boundary_discrete_geometry::SelectedKind::EdgeEdge{..}){return Err(fail("EE residual-gap allowance is not qualified"));}
        let allowance=super::boundary_path::ResidualPathAllowance::new(residual_tolerance,self.gap_scale).map_err(fail)?;
        let policy=allowance.policy(self.path_policy).map_err(fail)?;self.with_path_policy(policy)
    }
    pub fn residual_path_certificate(&self,previous:&[f64],current:&[f64],design:&[f64],residual_tolerance:f64)->CaeResult<super::boundary_path::ApproximatePathCertificate>{
        if matches!(self.kind,super::boundary_discrete_geometry::SelectedKind::EdgeEdge{..}){return Err(fail("EE residual-gap allowance is not qualified"));}
        let allowance=super::boundary_path::ResidualPathAllowance::new(residual_tolerance,self.gap_scale).map_err(fail)?;
        if self.path_policy.minimum_signed_gap.to_bits()!=(-allowance.allowance_m).to_bits(){return Err(fail("contact path allowance identity"));}
        if current.len()!=self.native_states+1||previous.len()!=self.native_states+1{return Err(fail("residual path complete state shape"));}
        if !current[self.native_states].is_finite()||current[self.native_states]<0.{return Err(fail("residual path nonnegative finite multiplier"));}
        let density=self.phases(design)?;let mut strict=self.path_policy;strict.minimum_signed_gap=0.;
        super::boundary_path::certify_residual_path(self.points(previous,&density)?,self.points(current,&density)?,strict,allowance).map_err(fail)
    }
    pub fn with_path_policy(mut self, policy: super::boundary_path::PathPolicy) -> CaeResult<Self> {
        if matches!(self.kind,super::boundary_discrete_geometry::SelectedKind::EdgeEdge{..})&&policy.minimum_signed_gap!=0.{return Err(fail("EE owner requires unchanged strict physical gap policy"));}
        if !(policy.minimum_barycentric >= 0. && policy.minimum_barycentric < 1./3.)
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
        for k in 0..4 {let b=self.bodies[k];points[k]=super::reference_embedding::position(&self.features[k].map(&density[b])?,self.references[k],&q[b])?;}
        Ok(points)
    }
    fn coordinates(&self, state: &[f64]) -> CaeResult<[Vec<[f64;3]>;2]> {
        if state.len() != self.native_states + 1 || state.iter().any(|v| !v.is_finite()) {
            return Err(fail("mapped complete state shape"));
        }
        let out: [Vec<[f64;3]>;2] = std::array::from_fn(|b| self.nodes[b].iter().map(|n| std::array::from_fn(|i| n.inverse_state_scale[i] * state[n.state[i]] + if self.density_surfaces.is_some(){n.reference_m[i]}else{0.})).collect());
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
        let rho=[values[..self.nodes[0].len()].to_vec(), values[self.nodes[0].len()..].to_vec()];if let Some(surfaces)=&self.density_surfaces{for b in 0..2{surfaces[b].require_fixed_stratum(&rho[b])?;}}Ok(rho)
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
        let map=MappedContact::evaluate_selected(self.kind,&self.features,self.bodies,self.references,&q,&q,&density)?;
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
        let map=MappedContact::evaluate_selected(self.kind,&self.features,self.bodies,self.references,&q,&q,&density)?;
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
        let map = MappedContact::evaluate_selected(self.kind,&self.features,self.bodies,self.references,&old,&new,&rho)?;
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
impl NativeContactLaw for BoundaryMappedPairLaw {
    fn check_derivative_domain(&self, _n: usize, current: &[f64], previous: &[f64], p: StepParameters<'_>) -> CaeResult<()> {
        let old=self.coordinates(previous)?;let new=self.coordinates(current)?;let density=self.phases(p.design)?;
        let map=MappedContact::evaluate_selected(self.kind,&self.features,self.bodies,self.references,&old,&new,&density)?;
        super::boundary_discrete_geometry::require_ordinary_feature_derivative(self.kind,self.points(previous,&density)?,self.points(current,&density)?,1e-6).map_err(|e|fail(&e))?;
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
        if matches!(self.kind,super::boundary_discrete_geometry::SelectedKind::VertexFace){
        let report=super::boundary_path::check_linear_path(self.points(previous,&density)?,self.points(current,&density)?,self.path_policy).map_err(fail)?;
        if report.status != super::boundary_path::PathStatus::Admitted {return Err(fail("complete contact path not admitted"));}
        }
        self.supporting_star_path(previous,current,&density)?;
        if current[self.native_states] < 0. {return Err(fail("negative converged contact multiplier"));}
        Ok(())
    }
    fn newton_constraints(&self, _n: usize, current: &[f64], previous: &[f64], p: StepParameters<'_>, direction: &[f64], attempt: usize) -> CaeResult<Option<Jacobian>> {
        let old=self.coordinates(previous)?;let new=self.coordinates(current)?;let density=self.phases(p.design)?;
        let map=MappedContact::evaluate_selected(self.kind,&self.features,self.bodies,self.references,&old,&new,&density)?;
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
        let map = MappedContact::evaluate_selected(self.kind,&self.features,self.bodies,self.references,&old,&new,&rho)?;
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
        for _ in 0..24{let candidate:Vec<_>=current.iter().zip(direction).map(|(x,d)|x+fraction*d).collect();let report=super::boundary_path::check_linear_path(old,self.points(&candidate,&density)?,self.path_policy).map_err(fail)?;if report.status==super::boundary_path::PathStatus::Admitted&&self.supporting_star_path(previous,&candidate,&density).is_ok(){return Ok(fraction)}fraction*=0.5;}Err(fail("boundary Newton path/cone not certified"))
    }
}