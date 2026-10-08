// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use super::{contact_field::ContactField, linear_path::PathPolicy, mapped_pair_law::{MappedPairLaw, NodeBinding}, surface_map::SurfaceFeature};
use crate::{interface::FluidField, model::FsiModel, problem::{self, FsiProblem}};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_physics_solid::soft_fsi::field::SoftSolidField;
use implexity_solve::multirate_coupling::{FluxDrivenField, MultirateOptions, MultirateStepper};
use serde_json::{Value, json};
use std::collections::BTreeSet;
fn fail(s: &str) -> CaeError { CaeError::contract(s) }
pub type MovingFsiStepper<'a> = MultirateStepper<FluidField, ContactField<SoftSolidField<'a>, MappedPairLaw>>;
#[derive(Clone)]
pub struct MovingContactSpec {
    pub body_nodes: [Vec<usize>; 2],
    pub features: [SurfaceFeature; 4],
    pub bodies: [usize; 4],
    pub replaced_planes: Vec<usize>,
    pub gap_scale_m: f64,
    pub force_scale_n: f64,
    pub path_policy: PathPolicy,
}
pub struct MovingFsiModel {
    native: FsiModel,
    contact: MovingContactSpec,
    identity: String,
}
fn feature_json(f: &SurfaceFeature) -> Value {
    match f {
        SurfaceFeature::Vertex(i) => json!({"vertex":i}),
        SurfaceFeature::FixedTetrahedron{nodes,weights,weight_tolerance}=>json!({"fixed_tetrahedron":nodes,"weights":weights,"weight_tolerance":weight_tolerance}),
        SurfaceFeature::DensityEdge {nodes,iso,contrast_min,interior_margin} =>
            json!({"density_edge":nodes,"iso":iso,"contrast_min":contrast_min,"interior_margin":interior_margin}),
    }
}
impl MovingFsiModel {
    pub fn new(original: FsiProblem, contact: MovingContactSpec) -> CaeResult<Self> {
        let mut normal = original.normal_form().clone();
        let planes = normal["solid"]["contact_planes"].as_array_mut().ok_or_else(||fail("normal contact plane array"))?;
        let selected: BTreeSet<_> = contact.replaced_planes.iter().copied().collect();
        if selected.len()!=contact.replaced_planes.len() || selected.iter().any(|i|*i>=planes.len()) {
            return Err(fail("invalid replaced contact plane selection"));
        }
        *planes=planes.iter().enumerate().filter(|(i,_)|!selected.contains(i)).map(|(_,v)|v.clone()).collect();
        let normalized=problem::normalise(&normal)?;
        let mut ids=BTreeSet::new();
        for nodes in &contact.body_nodes {
            if nodes.is_empty() || nodes.iter().any(|i|*i>=normalized.solid.grid.node_count() || !ids.insert(*i)) {
                return Err(fail("moving contact bodies need disjoint valid node indices"));
            }
        }
        let policy=contact.path_policy;
        let descriptor=json!({"schema":"implexity-fixed-feature-moving-fsi/1", "original_problem":original.identity(),
            "effective_problem":normalized.identity(),"body_nodes":contact.body_nodes,
            "features":contact.features.iter().map(feature_json).collect::<Vec<_>>(),"bodies":contact.bodies,
            "replaced_planes":contact.replaced_planes,"gap_scale_m":contact.gap_scale_m,"force_scale_n":contact.force_scale_n,
            "nodal_phase":"incident_voxel_arithmetic_mean",
            "path":{"barycentric":policy.minimum_barycentric,"area_ratio":policy.minimum_area_ratio,
                "signed_gap_m":policy.minimum_signed_gap,"time_resolution":policy.time_resolution,
                "maximum_intervals":policy.maximum_intervals,"maximum_depth":policy.maximum_depth}});
        let identity=implexity_core::json::canonical_sha256(&descriptor);
        Ok(Self {native:FsiModel::new(normalized)?,contact,identity})
    }
    pub fn native(&self) -> &FsiModel { &self.native }
    pub fn identity(&self) -> &str { &self.identity }
    pub fn contact_field(&self) -> CaeResult<ContactField<SoftSolidField<'_>,MappedPairLaw>> {
        let solid=self.native.solid_field()?;
        let grid=&self.native.problem.solid.grid;
        let scale=solid.core().scale();
        let mut nodes: [Vec<NodeBinding>;2]=std::array::from_fn(|_|Vec::new());
        let mut ri=Vec::new();let mut ci=Vec::new();let mut vs=Vec::new();let mut row=0;
        for (b, body) in self.contact.body_nodes.iter().enumerate() {
            for &id in body {
                let mut state=[0;3];let mut inverse=[0.;3];
                for a in 0..3 {
                    let (columns,values)=solid.trace_operator().row(3*id+a);
                    if columns.len()!=1 || values.len()!=1 || values[0]!=1. {
                        return Err(fail("moving contact needs nodal displacement trace rows"));
                    }
                    state[a]=columns[0];inverse[a]=1./scale[state[a]];
                }
                nodes[b].push(NodeBinding {reference_m:grid.node_position(id),state,inverse_state_scale:inverse,
                    force_flux:[3*id,3*id+1,3*id+2]});
                let ijk=grid.node_ijk(id);let mut incident=Vec::new();
                for x in 0..2 {for y in 0..2 {for z in 0..2 {
                    let shift=[x,y,z];let mut v=[0;3];let mut valid=true;
                    for a in 0..3 {
                        if ijk[a]<shift[a] {valid=false;break;}
                        v[a]=ijk[a]-shift[a];if v[a]>=grid.shape[a] {valid=false;break;}
                    }
                    if valid {incident.push(grid.voxel_index(v));}
                }}}
                if incident.is_empty() {return Err(fail("contact node has no incident material voxel"));}
                let weight=1./incident.len() as f64;
                for v in incident {ri.push(row);ci.push(v);vs.push(weight);}
                row+=1;
            }
        }
        let phase=CsrMatrix::from_triplets(row,grid.voxel_count(),&ri,&ci,&vs).map_err(|e|fail(&e.to_string()))?;
        let law=MappedPairLaw::new(nodes,self.contact.features.clone(),self.contact.bodies,phase,
            solid.state_size(),solid.trace_operator().nrows(),self.contact.gap_scale_m,self.contact.force_scale_n)?
            .with_path_policy(self.contact.path_policy)?;
        ContactField::new(solid,law)
    }
    pub fn stepper(&self)->CaeResult<MovingFsiStepper<'_>> {
        let c=&self.native.problem.coupling;
        Ok(MultirateStepper::new(self.native.fluid_field()?,self.contact_field()?,MultirateOptions {
            mode:c.mode.clone(),schur_ratio_limit:c.schur_ratio_limit,work_defect_limit:c.work_defect_limit})?
            .with_identity(format!("moving_contact_fsi:{}",self.identity))
            .with_field_newton(c.field_newton)?.with_linear_solves(c.linear_solves)?.with_step_cache_bytes(c.step_cache_bytes))
    }
}
