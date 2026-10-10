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

use crate::{
    gas_geometry_time::{GeometryTime, GeometryTimeBase},
    gas_layered_tetra_mesh::{self, LayeredGapTriangle},
    gas_multiscale_network::MultiscaleNetworkInputs,
    gas_transient_kinetic::{
        TransientGasBoundary, TransientGasFace, TransientGasInputs, TransientGasPacket,
        TransientGasPolicy, TransientGasScalar, TransientVelocityGrid,
        velocity_equilibrium_density,
    },
    native_cut_cell_partition::{CutWallQuadrature, NativeCutCellOwner},
    native_cut_cell_refinement::{self, CutTetraSubdomain, RefinedCutCellPartition},
    native_cut_cell_state::{AggregatedGasGeometry, NativeGasStateLayout},
    native_gas_wall_transfer::{NativeGasWallLoad, NativeGasWallTransfer},
    surface_gap::SurfaceGapKinematics,
};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use std::collections::BTreeMap;
pub(crate) mod corner_replay;
pub(crate) mod wall_support_construction;
pub(crate) use wall_support_construction::centred_centroid_bound;
mod layered_aperture_dispatch;
mod source_wall_support;
struct AuthenticatedApertureJets<S: TransientGasScalar> {
    apertures: Vec<Option<crate::native_cut_cell_partition::CutCellAperture<S>>>,
}
fn aperture_leaf_bits<S:TransientGasScalar>(a:S,b:S)->bool {
    a.value().to_bits()==b.value().to_bits() && match (a.geometry_first_order_directions(),b.geometry_first_order_directions()) {
        (Some(a),Some(b)) => a.len()==b.len() && a.iter().zip(b).all(|(a,b)|a.to_bits()==b.to_bits()),
        _=>false,
    }
}
fn aperture_polygon_bits<S:TransientGasScalar>(a:&[[S;3]],b:&[[S;3]])->bool {
    a.len()==b.len() && a.iter().zip(b).all(|(a,b)| (0..3).all(|k|aperture_leaf_bits(a[k],b[k])))
}
fn aperture_face_bits<S:TransientGasScalar>(a:&crate::native_cut_cell_partition::CutPolygon<S>,b:&crate::native_cut_cell_partition::CutPolygon<S>)->bool {
    a.boundary==b.boundary && a.support_only==b.support_only && aperture_polygon_bits(&a.vertices_m,&b.vertices_m)
    && (0..3).all(|k|aperture_leaf_bits(a.outward_normal[k],b.outward_normal[k]) && aperture_leaf_bits(a.plane_origin_m[k],b.plane_origin_m[k]))
    && aperture_leaf_bits(a.plane_offset_m,b.plane_offset_m)
}
fn aperture_owner_bits<S:TransientGasScalar>(a:&crate::native_cut_cell_partition::CutCellAperture<S>,b:&crate::native_cut_cell_partition::CutCellAperture<S>)->bool {
    a.left==b.left && a.right==b.right && a.outside_cell==b.outside_cell && a.axis==b.axis && a.polygon_parts==b.polygon_parts
    && (0..3).all(|k|aperture_leaf_bits(a.normal[k],b.normal[k])) && aperture_leaf_bits(a.area_m2,b.area_m2)
    && a.polygons.len()==b.polygons.len() && a.polygons.iter().zip(&b.polygons).all(|(a,b)|aperture_polygon_bits(a,b))
}
fn authenticate_convex_aperture<S:TransientGasScalar>(refined:&RefinedCutCellPartition<S>,actual:&crate::native_cut_cell_partition::CutCellAperture<S>,source:&crate::native_cut_cell_refinement::ApertureClipSource<S>)->CaeResult<()> {
    let(left,right,lp,rp,lf,rf)=source.owners();
    let a=refined.partition.components.get(left).and_then(|c|c.convex_parts.get(lp)).ok_or_else(||fail("facet original source part absent"))?;
    let b=refined.partition.components.get(right).and_then(|c|c.convex_parts.get(rp)).ok_or_else(||fail("facet original target part absent"))?;
    let source_face=a.get(lf).ok_or_else(||fail("facet original source face absent"))?;let target_face=b.get(rf).ok_or_else(||fail("facet original target face absent"))?;
    if source.macro_facet().is_some()||a.len()!=source.source().len()||b.len()!=source.target().len()||!a.iter().zip(source.source()).all(|(a,b)|aperture_face_bits(a,b))||!b.iter().zip(source.target()).all(|(a,b)|aperture_face_bits(a,b))||!aperture_owner_bits(actual,source.aperture())||source_face.support_only||target_face.support_only||actual.polygons.len()!=1||!aperture_polygon_bits(&source.replay()?,&actual.polygons[0]){return Err(fail("facet original clip full binding differs"));}Ok(())
}

impl<S:TransientGasScalar> AuthenticatedApertureJets<S> {
    fn issue(refined:&RefinedCutCellPartition<S>)->CaeResult<Self> { Self::issue_owned(refined,None) }
    fn issue_owned(refined:&RefinedCutCellPartition<S>,macro_owner:Option<(&[CutTetraSubdomain<S>],&crate::native_gap_column_owner::NativeGapColumnGrid)>)->CaeResult<Self> {
        if refined.aperture_sources.len()!=refined.partition.apertures.len(){return Err(fail("aperture clipping source count differs"));}
        let mut apertures=Vec::with_capacity(refined.partition.apertures.len());
        for (actual,source) in refined.partition.apertures.iter().zip(&refined.aperture_sources) {
            let Some(source)=source else {apertures.push(None);continue;};
            if let Some(facet)=source.macro_facet(){
                let(tets,grid)=macro_owner.ok_or_else(||fail("macro facet requires original macro source owner"))?;
                let actual_bounds=grid.cell_bounds(facet.cell)?;if !(0..2).all(|side|(0..3).all(|axis|actual_bounds[side][axis].to_bits()==facet.bounds[side][axis].to_bits())){return Err(fail("macro facet original grid bounds differ"));}
                let region=facet.regions.0.or(facet.regions.1).ok_or_else(||fail("macro facet original region absent"))?;
                let tet=tets.iter().find(|t|t.region==region).ok_or_else(||fail("macro facet original tetrahedron absent"))?;
                let ids=tet.vertex_ids.ok_or_else(||fail("macro facet original identifiers absent"))?;
                let faces=[[0,1,2],[0,1,3],[0,2,3],[1,2,3]];
                let face=faces.iter().position(|local|{let mut actual=local.map(|i|ids[i]);actual.sort();actual==facet.vertex_ids}).ok_or_else(||fail("macro facet original identifiers differ"))?;
                let mut triangle=native_cut_cell_refinement::canonical_face(tet,face)?;
                let u:[S;3]=std::array::from_fn(|k|triangle[1][k]-triangle[0][k]);let v:[S;3]=std::array::from_fn(|k|triangle[2][k]-triangle[0][k]);let n=[u[1]*v[2]-u[2]*v[1],u[2]*v[0]-u[0]*v[2],u[0]*v[1]-u[1]*v[0]];
                let remaining=(0..4).find(|i|!faces[face].contains(i)).unwrap();
                let inward=(0..3).map(|k|n[k]*(tet.vertices_m[remaining][k]-triangle[0][k])).fold(S::zero(),|a,b|a+b).value()>0.;if inward!=facet.regions.0.is_none(){triangle.swap(1,2);}
                if !aperture_polygon_bits(&triangle,&facet.triangle){return Err(fail("macro facet original scalar leaves differ"));}
                let other_region=if facet.regions.0==Some(region){facet.regions.1}else{facet.regions.0};
                let shared=tets.iter().filter(|t|t.region!=region).filter(|t|t.vertex_ids.is_some_and(|ids|facet.vertex_ids.iter().all(|id|ids.contains(id)))).collect::<Vec<_>>();
                if match other_region{Some(other)=>shared.len()!=1||shared[0].region!=other,None=>!shared.is_empty()}{return Err(fail("macro facet original neighboring ownership differs"));}
                for other in shared{let other_ids=other.vertex_ids.unwrap();for id in facet.vertex_ids{let a=ids.iter().position(|v|*v==id).unwrap();let b=other_ids.iter().position(|v|*v==id).unwrap();if !(0..3).all(|k|aperture_leaf_bits(tet.vertices_m[a][k],other.vertices_m[b][k])){return Err(fail("macro facet original neighboring scalar leaves differ"));}}}
                let right=actual.right.ok_or_else(||fail("macro facet right owner absent"))?;
                let regions=(*refined.regions.get(actual.left).ok_or_else(||fail("macro facet left region absent"))?,*refined.regions.get(right).ok_or_else(||fail("macro facet right region absent"))?);
                let left_cell=refined.partition.components.get(actual.left).ok_or_else(||fail("macro facet left component absent"))?.cell;
                let right_cell=refined.partition.components.get(right).ok_or_else(||fail("macro facet right component absent"))?.cell;
                if facet.vertex_ids.iter().enumerate().any(|(i,id)|facet.vertex_ids[i+1..].contains(id))||facet.regions!=regions||left_cell!=facet.cell||right_cell!=facet.cell||actual.axis.is_some()||!actual.polygon_parts.is_empty()||actual.polygons.len()!=1||!aperture_owner_bits(actual,source.aperture())||!aperture_polygon_bits(&source.replay()?,&actual.polygons[0]){return Err(fail("macro facet full source binding differs"));}
                for patch in &facet.patches{authenticate_convex_aperture(refined,patch.aperture(),patch)?;}
                apertures.push(Some(source.aperture().clone()));continue;
            }
            if actual.axis.is_none(){apertures.push(None);continue;}
            let (left,right,left_part,right_part,left_face,right_face)=source.owners();
            let source_part=refined.partition.components.get(left).and_then(|c|c.convex_parts.get(left_part)).ok_or_else(||fail("aperture actual source owner is absent"))?;
            let target_part=refined.partition.components.get(right).and_then(|c|c.convex_parts.get(right_part)).ok_or_else(||fail("aperture actual target owner is absent"))?;
            if source_part.len()!=source.source().len() || target_part.len()!=source.target().len()
                || !source_part.iter().zip(source.source()).all(|(a,b)|aperture_face_bits(a,b))
                || !target_part.iter().zip(source.target()).all(|(a,b)|aperture_face_bits(a,b))
                || actual.left!=left || actual.right!=Some(right) || actual.polygon_parts!=vec![(left_part,Some(right_part))]
                || !aperture_owner_bits(actual,source.aperture()) {
                return Err(fail("aperture clipping source full binding differs"));
            }
            let a=source_part.get(left_face).ok_or_else(||fail("aperture actual source face is absent"))?;
            let b=target_part.get(right_face).ok_or_else(||fail("aperture actual target face is absent"))?;
            let opposed=(0..3).map(|k|a.outward_normal[k]*b.outward_normal[k]).fold(S::zero(),|a,b|a+b);
            if a.support_only || b.support_only || opposed.value()>-0.5 || actual.polygons.len()!=1
                || !aperture_polygon_bits(&source.replay()?,&actual.polygons[0]) {
                return Err(fail("aperture clipping source replay differs"));
            }
            apertures.push(Some(source.aperture().clone()));
        }
        Ok(Self{apertures})
    }
    fn validate(&self,index:usize,actual:&crate::native_cut_cell_partition::CutCellAperture<S>)->CaeResult<bool> {
        match self.apertures.get(index).ok_or_else(||fail("aperture source receipt index is absent"))? {
            Some(expected)=>if aperture_owner_bits(actual,expected){Ok(true)}else{Err(fail("authenticated aperture jets differ"))},
            None=>Ok(false),
        }
    }
}
fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
pub(crate) fn material_barycentric<S: Scalar>(p: [S; 3], vertices: [[S; 3]; 4]) -> CaeResult<[S; 4]> {
    let sub = |a: [S; 3], b: [S; 3]| std::array::from_fn(|k| a[k] - b[k]);
    let cross = |a: [S; 3], b: [S; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let dot = |a: [S; 3], b: [S; 3]| (0..3).map(|k| a[k] * b[k]).fold(S::zero(), |a, b| a + b);
    let a = sub(vertices[1], vertices[0]);
    let b = sub(vertices[2], vertices[0]);
    let c = sub(vertices[3], vertices[0]);
    let d = sub(p, vertices[0]);
    let determinant = dot(a, cross(b, c));
    if !determinant.value().is_finite() || determinant.value() == 0. {
        return Err(fail("degenerate numerical gas material chart"));
    }
    let w = [
        dot(d, cross(b, c)) / determinant,
        dot(a, cross(d, c)) / determinant,
        dot(a, cross(b, d)) / determinant,
    ];
    Ok([
        S::one() - w.iter().copied().fold(S::zero(), |a, b| a + b),
        w[0],
        w[1],
        w[2],
    ])
}

#[derive(Clone, Copy, Debug)]
pub enum NativeGasExteriorBoundary {
    Port { port: usize },
    StationaryWall,
}
pub struct NativeGasGeometryAdapter {
    pub owner: NativeCutCellOwner,
    pub kinematics: SurfaceGapKinematics,
    pub paired_vertex_ids: Vec<[usize; 3]>,
    pub layers: usize,
    pub length_tolerance_m: f64,
    pub layout: NativeGasStateLayout,
}
pub(crate) struct NativeGasVolumePoint {
    identity: String,
}
pub(crate) struct LocalNativeGasVolumes<S: Scalar> {
    pub source_point_identity: String,
    pub cells: Vec<usize>,
    pub halo_cells: usize,
    pub slot_volumes_m3: Vec<(usize, S)>,
    pub partition_defects_m3: Vec<S>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum NativePointOwner {
    Native(usize, usize),
    Layered(usize),
}
#[derive(Clone, Debug)]
enum NativeCornerKind {
    Parent(NativePointOwner),
    Edge { left: NativePointOwner, right: NativePointOwner, plane: crate::native_cut_cell_partition::CutBoundary },
    PlaneIntersection { selected: [crate::native_cut_cell_partition::CutBoundary; 3], anchor: crate::native_cut_cell_partition::CutBoundary, delta: [f64; 3] },
}
#[derive(Clone, Debug)]
struct NativeCornerConstruction<S: Scalar> {
    cell: usize,
    retained: [f64; 3],
    actual: [S; 3],
    active: Vec<crate::native_cut_cell_partition::CutBoundary>,
    kind: NativeCornerKind,
    layered_sources: Vec<(usize, usize, usize, usize)>,
    tet_vertices: Vec<(usize, [usize; 4])>,
}
pub(crate) struct NativeSourceZeroFan<'a,S:Scalar>{adapter:&'a NativeGasGeometryAdapter,support:&'a NativeGasConvexSupport<S>,face:usize,fan:usize,pair:(usize,usize),observed:S}
impl<S:TransientGasScalar> NativeSourceZeroFan<'_,S>{
    pub(crate) fn validate_observed(&self,observed:S)->CaeResult<()>{
        let repeated=self.adapter.issue_native_source_zero_fan(self.support,self.face,self.fan)?.ok_or_else(||fail("native repeated-source fan authority is absent"))?;
        if repeated.pair!=self.pair || observed.value().to_bits()!=self.observed.value().to_bits() || observed.geometry_first_order_directions().map(|v|v.into_iter().map(f64::to_bits).collect::<Vec<_>>())!=self.observed.geometry_first_order_directions().map(|v|v.into_iter().map(f64::to_bits).collect::<Vec<_>>()){return Err(fail("native repeated-source fan scalar binding differs"));}
        Ok(())
    }
}
pub(crate) struct NativeOwnedWallFacet<'a,S:Scalar>{adapter:&'a NativeGasGeometryAdapter,support:&'a NativeGasConvexSupport<S>,quadrature:&'a CutWallQuadrature<S>,fan:usize,plane:usize}
impl<S:Scalar> NativeOwnedWallFacet<'_,S>{
    pub(crate) fn plane(&self)->usize{self.plane}
    pub(crate) fn plane_values(&self)->([S;3],[S;3],S){let p=&self.support.polygons[self.plane];(p.outward_normal,p.plane_origin_m,p.plane_offset_m)}
    pub(crate) fn normal(&self)->[S;3]{self.quadrature.normal}
    pub(crate) fn validate(&self)->CaeResult<()>{let repeated=self.adapter.issue_native_owned_wall_facet(self.support,self.quadrature,self.fan)?;if repeated.plane!=self.plane{return Err(fail("native owned wall plane identity differs"));}Ok(())}
}
pub struct NativeGasConvexSupport<S: Scalar> {
    pub(crate) component: usize,
    pub(crate) convex_part: usize,
    pub(crate) polygons: Vec<crate::native_cut_cell_partition::CutPolygon<S>>,
    corner_constructions: Vec<Vec<NativeCornerConstruction<S>>>,
    wall_constructions: Vec<NativeCornerConstruction<S>>,
    plane_tet_vertices: Vec<(usize, [usize;4])>,
    plane_layered_sources: Vec<(usize, usize, usize, usize)>,
}
impl<S: Scalar> NativeGasConvexSupport<S> {
    pub fn component(&self) -> usize { self.component }
    pub fn convex_part(&self) -> usize { self.convex_part }
    pub fn polygons(&self) -> &[crate::native_cut_cell_partition::CutPolygon<S>] { &self.polygons }
}
pub struct NativeGasGeometryEndpoint<S: Scalar> {
    pub refined: RefinedCutCellPartition<S>,
    pub aggregated: AggregatedGasGeometry<S>,
    pub partition_defects_m3: Vec<S>,
    pub wall_convex_support: Vec<NativeGasConvexSupport<S>>,
}
pub struct NativeGasWallStage<S: GeometryTimeBase> {
    pub transfer: NativeGasWallTransfer<S>,
    pub face_indices: Vec<usize>,
}
pub(crate) struct NativeBirthPathAuthority { geometry:std::sync::Arc<NativeGasGeometryAdapter>, sample:NativeBirthAuthoritySample }
enum NativeBirthAuthoritySample { Release(std::sync::Arc<crate::native_closed_release::NativeClosedReleaseSample>),Primal(std::sync::Arc<crate::native_closed_release::NativeClosedGeometryWitness>),Trial(std::sync::Arc<crate::native_closed_release::NativeClosedTrialSample>) }
impl NativeBirthPathAuthority {
    pub(crate) fn matches_release(&self,geometry:&NativeGasGeometryAdapter,sample:&crate::native_closed_release::NativeClosedReleaseSample)->bool{std::ptr::eq(self.geometry.as_ref(),geometry)&&matches!(&self.sample,NativeBirthAuthoritySample::Release(actual)if std::ptr::eq(actual.as_ref(),sample))}
}
pub struct NativeGasGeometryPath<S: GeometryTimeBase> {
    pub wall_interior_anchors: Vec<Option<crate::gas_quadratic_diffuse_wall::QuadraticWallInteriorAnchor<S>>>,
    wall_anchor_route_identity: String,
    birth_authority:Option<NativeBirthPathAuthority>,
    birth_input_leaves:Vec<S>,
    pub(crate) birth_regions:Vec<usize>,
    pub(crate) birth_route_identity:String,
    pub step_s: S,
    pub faces: Vec<TransientGasFace<S>>,
    pub left_centroid_to_face_m: Vec<[S; 3]>,
    pub right_centroid_to_face_m: Vec<Option<[S; 3]>>,
    pub old_geometry: NativeGasGeometryEndpoint<S>,
    pub current_geometry: NativeGasGeometryEndpoint<S>,
    pub wall_stages: Vec<NativeGasWallStage<S>>,
    pub external_wall_faces: Vec<usize>,
}
pub struct NativeGasGeometryAssembly<S: GeometryTimeBase> {
    pub step_s: S,
    pub previous_coefficient_inventory: Vec<S>,
    pub faces: Vec<TransientGasFace<S>>,
    pub left_centroid_to_face_m: Vec<[S; 3]>,
    pub right_centroid_to_face_m: Vec<Option<[S; 3]>>,
    pub old_geometry: NativeGasGeometryEndpoint<S>,
    pub current_geometry: NativeGasGeometryEndpoint<S>,
    pub wall_stages: Vec<NativeGasWallStage<S>>,
    pub external_wall_faces: Vec<usize>,
    pub maximum_remap_coefficient_defect: f64,
}
pub struct NativeGasTransportAssembly<S: GeometryTimeBase> {
    pub inputs: MultiscaleNetworkInputs<S>,
    pub old_geometry: NativeGasGeometryEndpoint<S>,
    pub current_geometry: NativeGasGeometryEndpoint<S>,
    pub wall_stages: Vec<NativeGasWallStage<S>>,
    pub external_wall_faces: Vec<usize>,
    pub maximum_remap_population_defect_kg: f64,
}
#[derive(Clone, Copy)]
enum NativeClosedGeometrySelection<'a> {
    Derivative(&'a crate::native_closed_release::NativeClosedReleaseCertificate),
    Primal(&'a crate::native_closed_release::NativeClosedGeometryWitness),
    Sample(&'a crate::native_closed_release::NativeClosedReleaseSample),
    Trial(&'a crate::native_closed_release::NativeClosedTrialSample),
}
pub(crate) struct NativeSourceClosedStage<'a> {
    adapter_identity: usize,
    selection: NativeClosedGeometrySelection<'a>,
    start_bits: Vec<Vec<u64>>,
    end_bits: Vec<Vec<u64>>,
    design_bits: Vec<Vec<u64>>,
    step_bits: Vec<Vec<u64>>,
    macro_time_bits: Option<Vec<Vec<u64>>>,
}
fn source_scalar_bits<S:TransientGasScalar>(values:&[S])->CaeResult<Vec<Vec<u64>>>{
    values.iter().map(|v|{
        if !v.finite_first_order(){return Err(fail("source runtime scalar is nonfinite"));}
        let directions=v.geometry_first_order_directions().ok_or_else(||fail("source runtime scalar channels are opaque"))?;
        if directions.iter().any(|d|!d.is_finite()){return Err(fail("source runtime direction is nonfinite"));}
        Ok(std::iter::once(v.value().to_bits()).chain(directions.into_iter().map(f64::to_bits)).collect())
    }).collect()
}
impl NativeSourceClosedStage<'_>{
    pub(crate) fn selected_charts<S:TransientGasScalar>(&self,adapter:&NativeGasGeometryAdapter,start:&[S],end:&[S],design:&[S],step_s:S,time_scale:f64)->CaeResult<(Vec<usize>,Vec<usize>)>{
        if self.adapter_identity!=adapter as *const NativeGasGeometryAdapter as usize
            ||self.start_bits!=source_scalar_bits(start)?||self.end_bits!=source_scalar_bits(end)?
            ||self.design_bits!=source_scalar_bits(design)?
            ||self.step_bits!=source_scalar_bits(&[step_s])?
            ||self.macro_time_bits.as_ref().is_some_and(|bits|bits[0][0]!=time_scale.to_bits()){
            return Err(fail("validated adapter source stage binding differs"));
        }
        Ok(match self.selection{
            NativeClosedGeometrySelection::Derivative(c)=>(c.closed_start_charts().to_vec(),c.closed_start_charts().iter().copied().filter(|&chart|(0..3).all(|corner|c.closed_endpoint_corners().contains(&(chart,corner)))).collect()),
            NativeClosedGeometrySelection::Primal(w)=>(w.closed_start_charts().to_vec(),w.closed_end_charts().to_vec()),
            NativeClosedGeometrySelection::Sample(s)=>(s.closed_start_charts().to_vec(),s.closed_end_charts().to_vec()),
            NativeClosedGeometrySelection::Trial(s)=>(s.closed_start_charts().to_vec(),s.closed_end_charts().to_vec()),
        })
    }
}


pub(crate) struct NativeProjectedChildReceipt<'a, S: GeometryTimeBase> {
    adapter: &'a NativeGasGeometryAdapter,
    sample: std::sync::Arc<crate::native_closed_release::NativeClosedReleaseSample>,
    macro_start: Vec<S>,
    macro_end: Vec<S>,
    child_start: Vec<S>,
    child_end: Vec<S>,
    design: Vec<S>,
    time_scale: S,
}
impl<'a, S: GeometryTimeBase> NativeProjectedChildReceipt<'a,S> {
    pub(crate) fn adapter(&self) -> &'a NativeGasGeometryAdapter { self.adapter }
    pub(crate) fn sample(&self) -> &std::sync::Arc<crate::native_closed_release::NativeClosedReleaseSample> { &self.sample }
    pub(crate) fn macro_start(&self) -> &[S] { &self.macro_start }
    pub(crate) fn macro_end(&self) -> &[S] { &self.macro_end }
    pub(crate) fn child_start(&self) -> &[S] { &self.child_start }
    pub(crate) fn child_end(&self) -> &[S] { &self.child_end }
    pub(crate) fn design(&self) -> &[S] { &self.design }
    pub(crate) fn time_scale(&self) -> S { self.time_scale }
}
pub(crate) struct NativeProjectedTrialChildReceipt<'a, S: GeometryTimeBase> {
    adapter: &'a NativeGasGeometryAdapter,
    sample: std::sync::Arc<crate::native_closed_release::NativeClosedTrialSample>,
    macro_start: Vec<S>,
    macro_end: Vec<S>,
    child_start: Vec<S>,
    child_end: Vec<S>,
    design: Vec<S>,
    time_scale: S,
}
impl<'a, S: GeometryTimeBase> NativeProjectedTrialChildReceipt<'a,S> {
    pub(crate) fn adapter(&self) -> &'a NativeGasGeometryAdapter { self.adapter }
    pub(crate) fn sample(&self) -> &std::sync::Arc<crate::native_closed_release::NativeClosedTrialSample> { &self.sample }
    pub(crate) fn macro_start(&self) -> &[S] { &self.macro_start }
    pub(crate) fn macro_end(&self) -> &[S] { &self.macro_end }
    pub(crate) fn child_start(&self) -> &[S] { &self.child_start }
    pub(crate) fn child_end(&self) -> &[S] { &self.child_end }
    pub(crate) fn design(&self) -> &[S] { &self.design }
    pub(crate) fn time_scale(&self) -> S { self.time_scale }
}
impl NativeGasGeometryAdapter {
    pub fn new(
        owner: NativeCutCellOwner,
        kinematics: SurfaceGapKinematics,
        paired_vertex_ids: Vec<[usize; 3]>,
        layers: usize,
        residual_slots_per_cell: usize,
        populations: usize,
        length_tolerance_m: f64,
    ) -> CaeResult<Self> {
        if paired_vertex_ids.is_empty()
            || layers == 0
            || !length_tolerance_m.is_finite()
            || length_tolerance_m < 0.
        {
            return Err(fail("invalid native gas geometry settings"));
        }
        kinematics.validate_native_owner_bindings(owner.capacity_owner())?;
        let regions = paired_vertex_ids
            .len()
            .checked_mul(layers)
            .and_then(|n| n.checked_mul(3))
            .ok_or_else(|| fail("native gas region count overflow"))?;
        let layout = NativeGasStateLayout::new(
            (0..regions).collect(),
            owner.capacity_owner().grid.cells.clone(),
            residual_slots_per_cell,
            populations,
        )?;
        Ok(Self {
            owner,
            kinematics,
            paired_vertex_ids,
            layers,
            length_tolerance_m,
            layout,
        })
    }
    pub(crate) fn retained_birth_release_authority<'a,S:GeometryTimeBase>(geometry:&std::sync::Arc<Self>,receipt:&NativeProjectedChildReceipt<'a,S>)->CaeResult<NativeBirthPathAuthority>{if !std::ptr::eq(geometry.as_ref(),receipt.adapter()){return Err(fail("retained birth authority belongs to another actual adapter"));}Ok(NativeBirthPathAuthority{geometry:geometry.clone(),sample:NativeBirthAuthoritySample::Release(receipt.sample().clone())})}
    pub(crate) fn validate_birth_release_owner<S:GeometryTimeBase>(&self,path:&NativeGasGeometryPath<S>,sample:&crate::native_closed_release::NativeClosedReleaseSample)->CaeResult<()>{if !path.birth_authority.as_ref().is_some_and(|owner|owner.matches_release(self,sample)){return Err(fail("birth path release sample identity differs"));}Ok(())}
    pub(crate) fn validate_birth_primal_owner<S:GeometryTimeBase>(&self,path:&NativeGasGeometryPath<S>,sample:&crate::native_closed_release::NativeClosedGeometryWitness)->CaeResult<()>{if !path.birth_authority.as_ref().is_some_and(|owner|std::ptr::eq(owner.geometry.as_ref(),self)&&matches!(&owner.sample,NativeBirthAuthoritySample::Primal(actual)if std::ptr::eq(actual.as_ref(),sample))){return Err(fail("birth path primal witness identity differs"));}Ok(())}
    pub(crate) fn validate_birth_trial_owner<S:GeometryTimeBase>(&self,path:&NativeGasGeometryPath<S>,sample:&crate::native_closed_release::NativeClosedTrialSample)->CaeResult<()>{if !path.birth_authority.as_ref().is_some_and(|owner|std::ptr::eq(owner.geometry.as_ref(),self)&&matches!(&owner.sample,NativeBirthAuthoritySample::Trial(actual)if std::ptr::eq(actual.as_ref(),sample))){return Err(fail("birth path trial sample identity differs"));}Ok(())}
    pub(crate) fn assemble_retained_birth_release_path<S:GeometryTimeBase>(geometry:&std::sync::Arc<Self>,start:&[S],end:&[S],design:&[S],time_scale:S,step_s:S,boundaries:&BTreeMap<(Option<usize>,usize,usize),NativeGasExteriorBoundary>,sample:std::sync::Arc<crate::native_closed_release::NativeClosedReleaseSample>)->CaeResult<NativeGasGeometryPath<S>>{let mut path=geometry.assemble_geometry_path_with_closed_sample(start,end,design,time_scale,step_s,boundaries,&sample)?;path.birth_authority=Some(NativeBirthPathAuthority{geometry:geometry.clone(),sample:NativeBirthAuthoritySample::Release(sample)});Ok(path)}
    pub(crate) fn assemble_retained_birth_trial_path<S:GeometryTimeBase>(geometry:&std::sync::Arc<Self>,start:&[S],end:&[S],design:&[S],time_scale:S,step_s:S,boundaries:&BTreeMap<(Option<usize>,usize,usize),NativeGasExteriorBoundary>,sample:std::sync::Arc<crate::native_closed_release::NativeClosedTrialSample>)->CaeResult<NativeGasGeometryPath<S>>{let mut path=geometry.assemble_geometry_path_with_closed_trial_sample(start,end,design,time_scale,step_s,boundaries,&sample)?;path.birth_authority=Some(NativeBirthPathAuthority{geometry:geometry.clone(),sample:NativeBirthAuthoritySample::Trial(sample)});Ok(path)}
    pub(crate) fn assemble_retained_birth_primal_path(geometry:&std::sync::Arc<Self>,start:&[f64],end:&[f64],design:&[f64],step_s:f64,boundaries:&BTreeMap<(Option<usize>,usize,usize),NativeGasExteriorBoundary>,sample:std::sync::Arc<crate::native_closed_release::NativeClosedGeometryWitness>)->CaeResult<NativeGasGeometryPath<f64>>{let mut path=geometry.assemble_geometry_path_with_closed_witness(start,end,design,step_s,boundaries,&sample)?;path.birth_authority=Some(NativeBirthPathAuthority{geometry:geometry.clone(),sample:NativeBirthAuthoritySample::Primal(sample)});Ok(path)}
    pub(crate) fn authenticated_birth_endpoints<'a,S:GeometryTimeBase>(&'a self,receipt:&NativeProjectedChildReceipt<'a,S>)->CaeResult<(NativeGasGeometryEndpoint<S>,NativeGasGeometryEndpoint<S>)>{
        if !std::ptr::eq(receipt.adapter(),self){return Err(fail("birth endpoint receipt belongs to another actual adapter"));}
        let selection=NativeClosedGeometrySelection::Sample(receipt.sample());
        let old_tets=self.chart_subdomains_selected(receipt.child_start(),receipt.design(),Some(selection),0.)?;
        let new_tets=self.chart_subdomains_selected(receipt.child_end(),receipt.design(),Some(selection),1.)?;
        Ok((self.endpoint_with_regions(receipt.child_start(),receipt.design(),&old_tets)?,self.endpoint_with_regions(receipt.child_end(),receipt.design(),&new_tets)?))
    }
    pub(crate) fn validate_birth_path_inputs<S:GeometryTimeBase>(&self,path:&NativeGasGeometryPath<S>,start:&[S],end:&[S],design:&[S],step_s:S)->CaeResult<()>{
        if !path.birth_authority.as_ref().is_some_and(|owner|std::ptr::eq(owner.geometry.as_ref(),self)){return Err(fail("birth path belongs to another actual geometry producer"));}
        let leaves=start.iter().chain(end).chain(design).copied().chain(std::iter::once(step_s)).collect::<Vec<_>>();
        Self::validate_birth_input_leaf_bits(&path.birth_input_leaves,&leaves)
    }
    pub(crate) fn validate_birth_input_leaf_bits<S:GeometryTimeBase>(owned:&[S],leaves:&[S])->CaeResult<()>{
        if leaves.len()!=owned.len(){return Err(fail("birth path input ownership shape differs"));}
        for (actual,expected) in owned.iter().zip(leaves){
            let a=actual.geometry_first_order_directions().ok_or_else(||fail("birth path full directions are unavailable"))?;
            let e=expected.geometry_first_order_directions().ok_or_else(||fail("birth sample full directions are unavailable"))?;
            if actual.value().to_bits()!=expected.value().to_bits()||a.len()!=e.len()||a.iter().zip(&e).any(|(a,e)|a.to_bits()!=e.to_bits()){return Err(fail("birth path differs from actual runtime child leaves or clock"));}
        }Ok(())
    }
    fn issue_native_source_zero_fan<'a,S:TransientGasScalar>(&'a self,support:&'a NativeGasConvexSupport<S>,face:usize,fan:usize)->CaeResult<Option<NativeSourceZeroFan<'a,S>>>{
        let polygon=support.polygons.get(face).ok_or_else(||fail("native repeated-source fan face is absent"))?;
        let recipes=support.corner_constructions.get(face).ok_or_else(||fail("native repeated-source fan recipes are absent"))?;
        if polygon.support_only || fan==0 || fan+1>=polygon.vertices_m.len() || recipes.len()!=polygon.vertices_m.len(){return Err(fail("native repeated-source fan incidence differs"));}
        let ids=[0,fan,fan+1];
        let mut pair=None;
        for a in 0..3{for b in a+1..3{
            if recipes[ids[a]].cell!=recipes[ids[b]].cell{continue;}
            let same=match (&recipes[ids[a]].kind,&recipes[ids[b]].kind){
                (NativeCornerKind::Parent(left),NativeCornerKind::Parent(right))=>left==right,
                (NativeCornerKind::Edge{left:al,right:ar,plane:ap},NativeCornerKind::Edge{left:bl,right:br,plane:bp})=>al==bl && ar==br && ap==bp,
                (NativeCornerKind::PlaneIntersection{selected:asel,anchor:aa,delta:ad},NativeCornerKind::PlaneIntersection{selected:bsel,anchor:ba,delta:bd})=>asel==bsel && aa==ba && (0..3).all(|k|ad[k].to_bits()==bd[k].to_bits()) && recipes[ids[a]].layered_sources==recipes[ids[b]].layered_sources && recipes[ids[a]].tet_vertices==recipes[ids[b]].tet_vertices,
                _=>false,
            };
            if !same{continue;}
            let validate_parent=|parent:&NativePointOwner|->CaeResult<()>{match parent{
                NativePointOwner::Native(body,node)=>{if self.owner.capacity_owner().nodes.get(*body).and_then(|nodes|nodes.get(*node)).is_none(){return Err(fail("native repeated-source parent is absent"));}},
                NativePointOwner::Layered(id)=>{let left=recipes[ids[a]].layered_sources.iter().find(|source|source.0==*id);let right=recipes[ids[b]].layered_sources.iter().find(|source|source.0==*id);if left.is_none() || left!=right{return Err(fail("native repeated-source layered source binding differs"));}},
            }Ok(())};
            match &recipes[ids[a]].kind{
                NativeCornerKind::Parent(parent)=>validate_parent(parent)?,
                NativeCornerKind::Edge{left,right,plane}=>{validate_parent(left)?;validate_parent(right)?;if !recipes[ids[a]].active.contains(plane)||!recipes[ids[b]].active.contains(plane){return Err(fail("native repeated-source edge plane incidence differs"));}},
                NativeCornerKind::PlaneIntersection{selected,anchor,delta}=>{
                    use crate::native_cut_cell_partition::CutBoundary;
                    if !selected.contains(anchor) || delta.iter().any(|v|!v.is_finite()){return Err(fail("native repeated-source intersection anchor differs"));}
                    for boundary in selected {
                        if !recipes[ids[a]].active.contains(boundary)||!recipes[ids[b]].active.contains(boundary){return Err(fail("native repeated-source intersection incidence differs"));}
                        let plane=support.polygons.iter().find(|plane|&plane.boundary==boundary).ok_or_else(||fail("native repeated-source intersection support plane absent"))?;
                        let values=plane.outward_normal.into_iter().chain(plane.plane_origin_m).chain([plane.plane_offset_m]);
                        for value in values {if !value.value().is_finite() || value.geometry_first_order_directions().is_none_or(|v|v.iter().any(|v|!v.is_finite())){return Err(fail("native repeated-source intersection plane leaves opaque"));}}
                        match boundary {
                            CutBoundary::Cell{axis,side}=>{if *axis>=3||*side>=2||!self.owner.capacity_owner().grid.cells.contains(&recipes[ids[a]].cell){return Err(fail("native repeated-source intersection cell ownership differs"));}},
                            CutBoundary::Native{body,element,face,..}=>{let nodes=self.owner.capacity_owner().nodes.get(*body).ok_or_else(||fail("native repeated-source intersection body absent"))?;let ids=self.owner.capacity_owner().elements.get(*body).and_then(|v|v.get(*element)).ok_or_else(||fail("native repeated-source intersection element absent"))?;if face.is_some_and(|face|face.iter().any(|id|!ids.contains(id)||nodes.get(*id).is_none())){return Err(fail("native repeated-source intersection finite face ownership differs"));}},
                            CutBoundary::Subdomain{region,face}=>{if *face>=4||recipes[ids[a]].tet_vertices.iter().all(|(id,_)|id!=region){return Err(fail("native repeated-source intersection tetra ownership differs"));}},
                        }
                    }
                },
                _=>return Err(fail("native repeated-source recipe is unsupported")),
            }
            for k in 0..3{
                let left=polygon.vertices_m[ids[a]][k];let right=polygon.vertices_m[ids[b]][k];
                if left.geometry_first_order_directions().is_none() || right.geometry_first_order_directions().is_none(){return Err(fail("native repeated-source point directions are opaque"));}
                if left.value().to_bits()!=right.value().to_bits() || left.geometry_first_order_directions().map(|v|v.into_iter().map(f64::to_bits).collect::<Vec<_>>())!=right.geometry_first_order_directions().map(|v|v.into_iter().map(f64::to_bits).collect::<Vec<_>>()){return Err(fail("native repeated-source full point binding differs"));}
                for id in [ids[a],ids[b]]{let actual=recipes[id].actual[k];let vertex=polygon.vertices_m[id][k];if actual.value().to_bits()!=vertex.value().to_bits() || actual.geometry_first_order_directions().map(|v|v.into_iter().map(f64::to_bits).collect::<Vec<_>>())!=vertex.geometry_first_order_directions().map(|v|v.into_iter().map(f64::to_bits).collect::<Vec<_>>()){return Err(fail("native repeated-source recipe binding differs"));}}
            }
            pair=Some((ids[a],ids[b]));
        }}
        let Some(pair)=pair else{return Ok(None)};
        let count=support.polygons.iter().filter(|p|!p.support_only).map(|p|p.vertices_m.len()).sum::<usize>();
        if count==0{return Err(fail("native repeated-source centre is absent"));}
        let centre:[S;3]=std::array::from_fn(|k|support.polygons.iter().filter(|p|!p.support_only).flat_map(|p|&p.vertices_m).map(|p|p[k]).fold(S::zero(),|a,b|a+b)/count as f64);
        let a:[S;3]=std::array::from_fn(|k|polygon.vertices_m[0][k]-centre[k]);let b:[S;3]=std::array::from_fn(|k|polygon.vertices_m[fan][k]-centre[k]);let c:[S;3]=std::array::from_fn(|k|polygon.vertices_m[fan+1][k]-centre[k]);
        let cross=[b[1]*c[2]-b[2]*c[1],b[2]*c[0]-b[0]*c[2],b[0]*c[1]-b[1]*c[0]];
        let observed=(0..3).map(|k|a[k]*cross[k]).fold(S::zero(),|a,b|a+b).abs();
        Ok(Some(NativeSourceZeroFan{adapter:self,support,face,fan,pair,observed}))
    }
    fn issue_native_owned_wall_facet<'a,S:Scalar>(&'a self,support:&'a NativeGasConvexSupport<S>,q:&'a CutWallQuadrature<S>,fan:usize)->CaeResult<NativeOwnedWallFacet<'a,S>>{
        if q.component!=support.component||fan+2>=support.wall_constructions.len(){return Err(fail("native wall facet fan owner differs"));}
        let ids=[0,fan+1,fan+2];let recipes=ids.map(|i|&support.wall_constructions[i]);
        let owner=self.owner.capacity_owner();
        let mut candidates=Vec::new();
        for (plane,p) in support.polygons.iter().enumerate(){
            let crate::native_cut_cell_partition::CutBoundary::Native{body,element,face:Some(face),physical:true}=p.boundary else{continue};
            if body!=q.body||owner.elements.get(body).and_then(|v|v.get(element))!=Some(&q.native_nodes)||!face.iter().all(|n|q.native_nodes.contains(n)){continue;}
            if recipes.iter().all(|r|r.active.contains(&p.boundary)){candidates.push(plane);}
        }
        if candidates.len()!=1{return Err(fail("native wall facet has ambiguous or lost physical incidence"));}
        let result=NativeOwnedWallFacet{adapter:self,support,quadrature:q,fan,plane:candidates[0]};
        Ok(result)
    }
    fn source_point_identity<S: Scalar>(&self, trace: &[S], design: &[S]) -> String {
        let owner = self.owner.capacity_owner();
        let source_point_identity = implexity_core::json::canonical_sha256(
            &serde_json::json!({"trace":trace.iter().map(|v|v.value()).collect::<Vec<_>>(),"design":design.iter().map(|v|v.value()).collect::<Vec<_>>(),"grid":owner.grid,"iso":owner.iso,"tolerance_m":owner.tolerance_m,"elements":owner.elements,"nodes":owner.nodes.iter().map(|b|b.iter().map(|n|serde_json::json!({"reference_m":n.reference_m,"force_flux":n.force_flux})).collect::<Vec<_>>()).collect::<Vec<_>>(),"phase":(0..owner.phase.nrows()).map(|r|{let(c,v)=owner.phase.row(r);serde_json::json!([c,v])}).collect::<Vec<_>>(),"paired_vertex_ids":self.paired_vertex_ids,"layers":self.layers,"length_tolerance_m":self.length_tolerance_m,"kinematic_binding":self.kinematics.binding_identity(),"layout_binding":self.layout.binding_identity()}),
        );
        source_point_identity
    }
    pub(crate) fn certify_volume_point(
        &self,
        trace: &[f64],
        design: &[f64],
    ) -> CaeResult<NativeGasVolumePoint> {
        self.endpoint(trace, design)?;
        Ok(NativeGasVolumePoint {
            identity: self.source_point_identity(trace, design),
        })
    }
    pub(crate) fn certify_initial_design_stratum(
        &self,
        trace: &[f64],
        design: &[f64],
    ) -> CaeResult<()> {
        use implexity_ad::Dual;
        let owner = self.owner.capacity_owner();
        if !owner
            .iso_face_design_columns(trace, design, &vec![1.; owner.grid.cells.len()])?
            .is_empty()
        {
            return Err(fail("initial native phase has a first-order interface tie"));
        }
        for columns in self.kinematics.active_design_indices()?.chunks(16) {
            let tr = trace
                .iter()
                .map(|&v| Dual::<16>::from_f64(v))
                .collect::<Vec<_>>();
            let mut x = design
                .iter()
                .map(|&v| Dual::<16>::from_f64(v))
                .collect::<Vec<_>>();
            for (k, &column) in columns.iter().enumerate() {
                x[column].eps[k] = 1.;
            }
            self.chart_subdomains(&tr, &x)?;
        }
        Ok(())
    }
    pub(crate) fn certify_initial_design_stratum_with_closed_release(
        &self,
        trace: &[f64],
        design: &[f64],
        closed: &crate::native_closed_release::NativeClosedReleaseCertificate,
    ) -> CaeResult<()> {
        use implexity_ad::Dual;
        let zero_trace = vec![0.; trace.len()];
        for &column in &self.kinematics.active_design_indices()? {
            let mut direction = vec![0.; design.len()];
            direction[column] = 1.;
            closed
                .validate_direction(&zero_trace, &zero_trace, Some(&direction), 0.)
                .map_err(|_| {
                    fail("initial fixed trace design basis leaves the maintained closed stratum")
                })?;
        }
        let owner = self.owner.capacity_owner();
        if !owner
            .iso_face_design_columns(trace, design, &vec![1.; owner.grid.cells.len()])?
            .is_empty()
        {
            return Err(fail("initial native phase has a first-order interface tie"));
        }
        for columns in self.kinematics.active_design_indices()?.chunks(16) {
            let tr = trace
                .iter()
                .map(|&v| Dual::<16>::from_f64(v))
                .collect::<Vec<_>>();
            let mut x = design
                .iter()
                .map(|&v| Dual::<16>::from_f64(v))
                .collect::<Vec<_>>();
            for (k, &column) in columns.iter().enumerate() {
                x[column].eps[k] = 1.;
            }
            let (tr, _) =
                self.project_closed_geometry_trace(&tr, &tr, &x, Dual::<16>::constant(1.), closed)?;
            self.chart_subdomains_selected(
                &tr,
                &x,
                Some(NativeClosedGeometrySelection::Derivative(closed)),
                0.,
            )?;
        }
        Ok(())
    }
    pub(crate) fn certify_volume_point_with_closed_release(
        &self,
        trace: &[f64],
        design: &[f64],
        closed: &crate::native_closed_release::NativeClosedReleaseCertificate,
    ) -> CaeResult<NativeGasVolumePoint> {
        closed.validate_time_scale(1.)?;
        closed.validate_path(&self.kinematics, trace, trace, design)?;
        let tets = self.chart_subdomains_selected(
            trace,
            design,
            Some(NativeClosedGeometrySelection::Derivative(closed)),
            0.,
        )?;
        self.endpoint_with_regions(trace, design, &tets)?;
        Ok(NativeGasVolumePoint {
            identity: self.source_point_identity(trace, design),
        })
    }
    pub(crate) fn volume_design_support(
        &self,
        certificate: &NativeGasVolumePoint,
        cells: &[usize],
        trace: &[f64],
        design: &[f64],
    ) -> CaeResult<Vec<usize>> {
        self.volume_design_support_selected(certificate, cells, trace, design, None)
    }
    pub(crate) fn volume_design_support_selected(
        &self,
        certificate: &NativeGasVolumePoint,
        cells: &[usize],
        trace: &[f64],
        design: &[f64],
        closed: Option<&crate::native_closed_release::NativeClosedReleaseCertificate>,
    ) -> CaeResult<Vec<usize>> {
        if certificate.identity != self.source_point_identity(trace, design) {
            return Err(fail("stale native gas volume point"));
        }
        let owner = self.owner.capacity_owner();
        owner.restricted_cells(cells)?;
        let bounds = cells
            .iter()
            .map(|&cell| owner.grid.cell_bounds(cell))
            .collect::<CaeResult<Vec<_>>>()?;
        let touches = |vertices: [[f64; 3]; 4]| {
            let low: [f64; 3] = std::array::from_fn(|axis| {
                vertices
                    .iter()
                    .map(|p| p[axis])
                    .fold(f64::INFINITY, f64::min)
            });
            let high: [f64; 3] = std::array::from_fn(|axis| {
                vertices
                    .iter()
                    .map(|p| p[axis])
                    .fold(f64::NEG_INFINITY, f64::max)
            });
            bounds
                .iter()
                .any(|b| (0..3).all(|axis| low[axis] <= b[1][axis] && high[axis] >= b[0][axis]))
        };
        let mut columns = std::collections::BTreeSet::new();
        for body in 0..2 {
            let offset = if body == 0 { 0 } else { owner.nodes[0].len() };
            for element in &owner.elements[body] {
                let vertices = element.map(|node| {
                    std::array::from_fn(|axis| {
                        owner.nodes[body][node].reference_m[axis]
                            + trace[owner.nodes[body][node].force_flux[axis]]
                    })
                });
                if touches(vertices) {
                    for &node in element {
                        let (indices, weights) = owner.phase.row(offset + node);
                        columns.extend(
                            indices
                                .iter()
                                .zip(weights)
                                .filter_map(|(&index, &weight)| (weight != 0.).then_some(index)),
                        );
                    }
                }
            }
        }
        let charts: std::collections::BTreeSet<_> = self
            .chart_subdomains_selected(
                trace,
                design,
                closed.map(NativeClosedGeometrySelection::Derivative),
                0.,
            )?
            .iter()
            .filter(|tet| touches(tet.vertices_m))
            .map(|tet| tet.region / (self.layers * 3))
            .collect();
        columns.extend(
            self.kinematics
                .active_design_indices_for_cells(&charts.into_iter().collect::<Vec<_>>())?,
        );
        if closed.is_some() {
            columns.extend(self.kinematics.active_design_indices()?);
        }
        Ok(columns.into_iter().collect())
    }
    pub(crate) fn volumes_on_cells<S: TransientGasScalar>(
        &self,
        certificate: &NativeGasVolumePoint,
        cells: &[usize],
        trace: &[S],
        design: &[S],
    ) -> CaeResult<LocalNativeGasVolumes<S>> {
        self.volumes_on_cells_selected(certificate, cells, trace, design, None)
    }
    pub(crate) fn volumes_on_cells_selected<S: TransientGasScalar>(
        &self,
        certificate: &NativeGasVolumePoint,
        cells: &[usize],
        trace: &[S],
        design: &[S],
        closed: Option<&crate::native_closed_release::NativeClosedReleaseCertificate>,
    ) -> CaeResult<LocalNativeGasVolumes<S>> {
        let source_point_identity = self.source_point_identity(trace, design);
        if certificate.identity != source_point_identity {
            return Err(fail("stale native gas volume point"));
        }
        self.owner.capacity_owner().restricted_cells(cells)?;
        let central: std::collections::BTreeSet<_> = cells.iter().copied().collect();
        let owner = self.owner.capacity_owner();
        let all_tets = self.chart_subdomains_selected(
            trace,
            design,
            closed.map(NativeClosedGeometrySelection::Derivative),
            0.,
        )?;
        let mut halo = central.clone();
        let mut selected = Vec::new();
        for tet in all_tets {
            let low: [f64; 3] = std::array::from_fn(|axis| {
                tet.vertices_m
                    .iter()
                    .map(|p| p[axis].value())
                    .fold(f64::INFINITY, f64::min)
            });
            let high: [f64; 3] = std::array::from_fn(|axis| {
                tet.vertices_m
                    .iter()
                    .map(|p| p[axis].value())
                    .fold(f64::NEG_INFINITY, f64::max)
            });
            let slots: Vec<_> = owner
                .bounds
                .iter()
                .enumerate()
                .filter_map(|(slot, bounds)| {
                    (0..3)
                        .all(|axis| low[axis] <= bounds[1][axis] && high[axis] >= bounds[0][axis])
                        .then_some(slot)
                })
                .collect();
            if slots
                .iter()
                .any(|&slot| central.contains(&owner.grid.cells[slot]))
            {
                halo.extend(slots.into_iter().map(|slot| owner.grid.cells[slot]));
                selected.push(tet);
            }
        }
        let halo: Vec<_> = halo.into_iter().collect();
        let restricted = Self {
            owner: NativeCutCellOwner::from_capacity_owner(owner.restricted_cells(&halo)?),
            kinematics: self.kinematics.clone(),
            paired_vertex_ids: self.paired_vertex_ids.clone(),
            layers: self.layers,
            length_tolerance_m: self.length_tolerance_m,
            layout: self.layout.clone(),
        };
        let endpoint = restricted.endpoint_with_regions(trace, design, &selected)?;
        let mut volumes = BTreeMap::new();
        for (component, &slot) in endpoint
            .refined
            .partition
            .components
            .iter()
            .zip(&endpoint.aggregated.component_slots)
        {
            if central.contains(&component.cell) {
                *volumes.entry(slot).or_insert(S::zero()) += component.volume_m3;
            }
        }
        Ok(LocalNativeGasVolumes {
            source_point_identity,
            cells: cells.to_vec(),
            halo_cells: halo.len(),
            slot_volumes_m3: volumes.into_iter().collect(),
            partition_defects_m3: endpoint.partition_defects_m3,
        })
    }
    pub(crate) fn checked_volume_design_pullback(
        &self,
        certificate: &NativeGasVolumePoint,
        cells: &[usize],
        trace: &[f64],
        design: &[f64],
        columns: &[usize],
        slot_bars: &[f64],
    ) -> CaeResult<Vec<f64>> {
        use crate::gas_vjp::{ReverseScalar, scoped_geometry_pullback};
        use implexity_ad::Dual;
        if columns.len() > 48
            || columns.windows(2).any(|w| w[0] >= w[1])
            || columns.iter().any(|&c| c >= design.len())
            || slot_bars.len() != self.layout.slots()
            || slot_bars.iter().any(|v| !v.is_finite())
        {
            return Err(fail("invalid local native volume pullback"));
        }
        let support = self.volume_design_support(certificate, cells, trace, design)?;
        if columns.iter().any(|c| support.binary_search(c).is_err()) {
            return Err(fail("local native volume derivative outside support"));
        }
        let owner = self.owner.capacity_owner();
        let active = owner
            .grid
            .cells
            .iter()
            .map(|cell| {
                if cells.binary_search(cell).is_ok() {
                    1.
                } else {
                    0.
                }
            })
            .collect::<Vec<_>>();
        let ties = owner.iso_face_design_columns(trace, design, &active)?;
        if columns.iter().any(|c| ties.contains(c)) {
            return Err(fail("local native volume derivative crosses a phase tie"));
        }
        let coordinate_scale = owner
            .grid
            .origin_m
            .iter()
            .map(|v| v.abs())
            .fold(owner.grid.spacing_m, f64::max);
        for base in (0..columns.len()).step_by(16) {
            let tr = trace
                .iter()
                .copied()
                .map(Dual::<16>::constant)
                .collect::<Vec<_>>();
            let mut x = design
                .iter()
                .copied()
                .map(Dual::<16>::constant)
                .collect::<Vec<_>>();
            for (j, &column) in columns.iter().skip(base).take(16).enumerate() {
                x[column].eps[j] = 1.;
            }
            let local = self.volumes_on_cells(certificate, cells, &tr, &x)?;
            let roundoff = 8192.
                * f64::EPSILON
                * (coordinate_scale / owner.grid.spacing_m).max(1.)
                * (1. + local.halo_cells as f64)
                * owner.grid.spacing_m.powi(3);
            for defect in local.partition_defects_m3 {
                if defect
                    .eps
                    .iter()
                    .any(|v| !v.is_finite() || v.abs() > roundoff)
                {
                    return Err(fail(
                        "local native volume derivative violates capacity partition",
                    ));
                }
            }
        }
        let values = columns.iter().map(|&c| design[c]).collect::<Vec<_>>();
        scoped_geometry_pullback(&values, |leaves| {
            let tr = trace
                .iter()
                .copied()
                .map(ReverseScalar::from_f64)
                .collect::<Vec<_>>();
            let mut x = design
                .iter()
                .copied()
                .map(ReverseScalar::from_f64)
                .collect::<Vec<_>>();
            for (&column, &value) in columns.iter().zip(leaves) {
                x[column] = value;
            }
            let local = self.volumes_on_cells(certificate, cells, &tr, &x)?;
            Ok(local
                .slot_volumes_m3
                .iter()
                .map(|&(slot, v)| v * slot_bars[slot])
                .fold(ReverseScalar::from_f64(0.), |a, b| a + b))
        })
    }
    pub(crate) fn checked_volume_design_pullback_with_closed_release(
        &self,
        certificate: &NativeGasVolumePoint,
        cells: &[usize],
        trace: &[f64],
        design: &[f64],
        columns: &[usize],
        slot_bars: &[f64],
        closed: &crate::native_closed_release::NativeClosedReleaseCertificate,
    ) -> CaeResult<Vec<f64>> {
        use crate::gas_vjp::{ReverseScalar, scoped_geometry_pullback};
        use implexity_ad::Dual;
        if columns.len() > 48
            || columns.windows(2).any(|w| w[0] >= w[1])
            || columns.iter().any(|&c| c >= design.len())
            || slot_bars.len() != self.layout.slots()
            || slot_bars.iter().any(|v| !v.is_finite())
        {
            return Err(fail("invalid local native volume pullback"));
        }
        let support =
            self.volume_design_support_selected(certificate, cells, trace, design, Some(closed))?;
        if columns.iter().any(|c| support.binary_search(c).is_err()) {
            return Err(fail("local native volume derivative outside support"));
        }
        let owner = self.owner.capacity_owner();
        let active = owner
            .grid
            .cells
            .iter()
            .map(|cell| {
                if cells.binary_search(cell).is_ok() {
                    1.
                } else {
                    0.
                }
            })
            .collect::<Vec<_>>();
        let ties = owner.iso_face_design_columns(trace, design, &active)?;
        if columns.iter().any(|c| ties.contains(c)) {
            return Err(fail("local native volume derivative crosses a phase tie"));
        }
        let coordinate_scale = owner
            .grid
            .origin_m
            .iter()
            .map(|v| v.abs())
            .fold(owner.grid.spacing_m, f64::max);
        for base in (0..columns.len()).step_by(16) {
            let tr = trace
                .iter()
                .copied()
                .map(Dual::<16>::constant)
                .collect::<Vec<_>>();
            let mut x = design
                .iter()
                .copied()
                .map(Dual::<16>::constant)
                .collect::<Vec<_>>();
            for (j, &column) in columns.iter().skip(base).take(16).enumerate() {
                x[column].eps[j] = 1.;
            }
            let (tr, _) =
                self.project_closed_geometry_trace(&tr, &tr, &x, Dual::<16>::constant(1.), closed)?;
            let local =
                self.volumes_on_cells_selected(certificate, cells, &tr, &x, Some(closed))?;
            let roundoff = 8192.
                * f64::EPSILON
                * (coordinate_scale / owner.grid.spacing_m).max(1.)
                * (1. + local.halo_cells as f64)
                * owner.grid.spacing_m.powi(3);
            for defect in local.partition_defects_m3 {
                if defect
                    .eps
                    .iter()
                    .any(|v| !v.is_finite() || v.abs() > roundoff)
                {
                    return Err(fail(
                        "local native volume derivative violates capacity partition",
                    ));
                }
            }
        }
        let values = columns.iter().map(|&c| design[c]).collect::<Vec<_>>();
        scoped_geometry_pullback(&values, |leaves| {
            let tr = trace
                .iter()
                .copied()
                .map(ReverseScalar::from_f64)
                .collect::<Vec<_>>();
            let mut x = design
                .iter()
                .copied()
                .map(ReverseScalar::from_f64)
                .collect::<Vec<_>>();
            for (&column, &value) in columns.iter().zip(leaves) {
                x[column] = value;
            }
            let (tr, _) = self.project_closed_geometry_trace(
                &tr,
                &tr,
                &x,
                ReverseScalar::from_f64(1.),
                closed,
            )?;
            let local =
                self.volumes_on_cells_selected(certificate, cells, &tr, &x, Some(closed))?;
            Ok(local
                .slot_volumes_m3
                .iter()
                .map(|&(slot, v)| v * slot_bars[slot])
                .fold(ReverseScalar::from_f64(0.), |a, b| a + b))
        })
    }
    fn canonicalize_source_facets<S:TransientGasScalar>(&self,refined:&mut RefinedCutCellPartition<S>,tets:&[CutTetraSubdomain<S>])->CaeResult<()> {
        use crate::native_cut_cell_partition;
        use crate::native_cut_cell_partition::{CutBoundary,CutCellAperture,Plane};
        use crate::native_cut_cell_refinement::ApertureClipSource;
        let mut groups:std::collections::BTreeMap<(usize,usize,Option<(usize,usize)>),Vec<usize>>=std::collections::BTreeMap::new();
        for (index,a) in refined.partition.apertures.iter().enumerate(){if a.axis.is_none(){if let Some(right)=a.right{if refined.regions[a.left].is_some()||refined.regions[right].is_some(){let owner=match(refined.regions[a.left],refined.regions[right]){(Some(region),None)|(None,Some(region))=>refined.aperture_sources[index].as_ref().and_then(|source|{let(_,_,_,_,lf,rf)=source.owners();match(source.source()[lf].boundary.clone(),source.target()[rf].boundary.clone()){(CutBoundary::Subdomain{region:r,face},CutBoundary::Subdomain{region:s,face:g})if r==region&&s==r&&g==face=>Some((r,face)),_=>None}}),_=>None};groups.entry((a.left,right,owner)).or_default().push(index);}}}}
        let mut replacements=std::collections::BTreeMap::new();let mut discarded=std::collections::BTreeSet::new();
        let face_indices=[[0,1,2],[0,1,3],[0,2,3],[1,2,3]];
        for ((left,right,boundary_owner),indices) in groups {
            let regions=(refined.regions[left],refined.regions[right]);
            if refined.partition.components[left].cell!=refined.partition.components[right].cell{return Err(fail("macro facet component cells differ"));}
            let cell=refined.partition.components[left].cell;
            let Some(region)=regions.0.or(regions.1) else{continue;};
            let tet=tets.iter().find(|t|t.region==region).ok_or_else(||fail("macro facet actual region is absent"))?;
            let Some(ids)=tet.vertex_ids else{continue;};
            let mut candidates=Vec::new();
            for (face,local) in face_indices.iter().enumerate(){
                let mut face_ids=local.map(|i|ids[i]);face_ids.sort();
                let mut shared=Vec::new();
                for other in tets.iter().filter(|t|t.region!=region){if let Some(other_ids)=other.vertex_ids{for (other_face,other_local) in face_indices.iter().enumerate(){let mut f=other_local.map(|i|other_ids[i]);f.sort();if f==face_ids{shared.push((other,other_face));}}}}
                if boundary_owner.is_some_and(|owner|owner!=(region,face)){continue;}
                let admitted=match (regions.0,regions.1){(Some(a),Some(b))=>shared.len()==1&&shared[0].0.region==if region==a{b}else{a},_=>shared.is_empty()};
                if !admitted{continue;}
                if shared.iter().any(|(other,_)|face_ids.iter().any(|id|{let a=ids.iter().position(|v|v==id).unwrap();let b=other.vertex_ids.unwrap().iter().position(|v|v==id).unwrap();!(0..3).all(|k|aperture_leaf_bits(tet.vertices_m[a][k],other.vertices_m[b][k]))})){return Err(fail("macro facet shared source leaves differ"));}
                if regions.0.is_none()||regions.1.is_none(){
                    if !indices.iter().all(|index|refined.aperture_sources[*index].as_ref().is_some_and(|source|{let(_,_,_,_,lf,rf)=source.owners();source.source().iter().any(|p|p.boundary==CutBoundary::Subdomain{region,face})&&source.target().iter().any(|p|p.boundary==CutBoundary::Subdomain{region,face})})){continue;}
                }
                candidates.push((face,face_ids));
            }
            if candidates.len()!=1{continue;}
            let (face,face_ids)=candidates[0];
            let mut triangle=native_cut_cell_refinement::canonical_face(tet,face)?;
            let remaining=(0..4).find(|i|!face_indices[face].contains(i)).unwrap();
            let sub=|a:[S;3],b:[S;3]|-> [S;3]{std::array::from_fn(|k|a[k]-b[k])};
            let u=sub(triangle[1],triangle[0]);let v=sub(triangle[2],triangle[0]);let mut n=[u[1]*v[2]-u[2]*v[1],u[2]*v[0]-u[0]*v[2],u[0]*v[1]-u[1]*v[0]];
            let inward=(0..3).map(|k|n[k]*(tet.vertices_m[remaining][k]-triangle[0][k])).fold(S::zero(),|a,b|a+b).value()>0.;
            if inward != regions.0.is_none(){triangle.swap(1,2);n=n.map(|v|-v);}
            let norm=(0..3).map(|k|n[k]*n[k]).fold(S::zero(),|a,b|a+b).sqrt();if norm.value()<=0.{return Err(fail("macro facet source is rank deficient"));}let normal=n.map(|v|v/norm);
            let bounds=self.owner.capacity_owner().grid.cell_bounds(cell)?;let mut polygon=triangle.to_vec();
            for axis in 0..3{for side in 0..2{let plane=Plane{normal:std::array::from_fn(|k|S::from_f64(if k==axis{if side==0{1.}else{-1.}}else{0.})),origin:bounds[side].map(S::from_f64),offset:S::zero(),boundary:CutBoundary::Cell{axis,side}};polygon=native_cut_cell_partition::clip_polygon(&polygon,&plane,refined.partition.arithmetic_length_m)?;}}
            if !native_cut_cell_partition::positive_polygon(&polygon,refined.partition.arithmetic_length_m){continue;}
            let measure=|polygons:&[Vec<[S;3]>]|->[f64;13]{let mut out=[0.;13];for p in polygons{for i in 1..p.len().saturating_sub(1){let q=[p[0],p[i],p[i+1]];let u=sub(q[1],q[0]);let v=sub(q[2],q[0]);let c=[u[1]*v[2]-u[2]*v[1],u[2]*v[0]-u[0]*v[2],u[0]*v[1]-u[1]*v[0]];let sign=if (0..3).map(|k|c[k]*normal[k]).fold(S::zero(),|a,b|a+b).value()<0.{-1.}else{1.};out[0]+=(0..3).map(|k|c[k]*c[k]).fold(S::zero(),|a,b|a+b).sqrt().value()*0.5;for k in 0..3{let vector=c[k].value()*0.5*sign;out[k+1]+=vector;for j in 0..3{out[4+k*3+j]+=vector*((q[0][j].value()-triangle[0][j].value())+(q[1][j].value()-triangle[0][j].value())+(q[2][j].value()-triangle[0][j].value()))/3.;}}}}out};
            let expected=measure(&[polygon.clone()]);let mut actual=[0.;13];let mut patches=Vec::new();
            for index in &indices{let a=&refined.partition.apertures[*index];let source=refined.aperture_sources[*index].as_ref().ok_or_else(||fail("macro facet original clip source is absent"))?;authenticate_convex_aperture(refined,a,source)?;let m=measure(&a.polygons);for k in 0..13{actual[k]+=m[k];}patches.push(source.clone());}
            let all_polygons=std::iter::once(&polygon).chain(indices.iter().flat_map(|index|refined.partition.apertures[*index].polygons.iter())).collect::<Vec<_>>();
            let perimeter=all_polygons.iter().map(|p|(0..p.len()).map(|i|(0..3).map(|k|{let d=p[i][k].value()-p[(i+1)%p.len()][k].value();d*d}).sum::<f64>().sqrt()).sum::<f64>()).sum::<f64>().next_up();
            let radius=all_polygons.iter().flat_map(|p|p.iter()).flat_map(|p|(0..3).map(|k|(p[k].value()-triangle[0][k].value()).abs())).fold(0f64,f64::max).next_up();
            let area_scale=(expected[0].abs()+actual[0].abs()).next_up();
            let area_bound=(4096.*f64::EPSILON*area_scale+8.*refined.partition.arithmetic_length_m*perimeter).next_up();
            if (0..13).any(|k|{let bound=if k<4{area_bound}else{(area_bound*radius+area_scale*8.*refined.partition.arithmetic_length_m+4096.*f64::EPSILON*(expected[k].abs()+actual[k].abs())).next_up()};(expected[k]-actual[k]).abs()>bound}){return Err(fail("macro facet union does not cover the admitted source face"));}
            let aperture=CutCellAperture{left,right:Some(right),outside_cell:None,axis:None,normal,area_m2:native_cut_cell_partition::area(&polygon),polygons:vec![polygon],polygon_parts:vec![]};
            let source=ApertureClipSource::from_macro_facet(patches,triangle,face_ids,regions,cell,bounds,aperture.clone())?;
            replacements.insert(indices[0],(aperture,source));discarded.extend(indices.iter().skip(1).copied());
        }
        let old_apertures=std::mem::take(&mut refined.partition.apertures);let old_sources=std::mem::take(&mut refined.aperture_sources);
        for (index,(a,s)) in old_apertures.into_iter().zip(old_sources).enumerate(){if discarded.contains(&index){continue;}if let Some((a,s))=replacements.remove(&index){refined.partition.apertures.push(a);refined.aperture_sources.push(Some(s));}else{refined.partition.apertures.push(a);refined.aperture_sources.push(s);}}
        Ok(())
    }

    pub fn endpoint<S: TransientGasScalar>(
        &self,
        trace: &[S],
        design: &[S],
    ) -> CaeResult<NativeGasGeometryEndpoint<S>> {
        let tets = self.chart_subdomains(trace, design)?;
        self.endpoint_with_regions(trace, design, &tets)
    }
    fn chart_subdomains<S: TransientGasScalar>(
        &self,
        trace: &[S],
        design: &[S],
    ) -> CaeResult<Vec<CutTetraSubdomain<S>>> {
        self.chart_subdomains_selected(trace, design, None, 0.)
    }
    fn chart_subdomains_selected<S: TransientGasScalar>(
        &self,
        trace: &[S],
        design: &[S],
        closed_release: Option<NativeClosedGeometrySelection<'_>>,
        fraction: f64,
    ) -> CaeResult<Vec<CutTetraSubdomain<S>>> {
        let vertices = self
            .kinematics
            .mapped_geometry_from_trace(trace, design, |v, _| Ok(v))?;
        if vertices.len() != self.paired_vertex_ids.len() {
            return Err(fail("native gas chart metadata shape"));
        }
        let triangles: Vec<_> = vertices
            .iter()
            .zip(&self.paired_vertex_ids)
            .map(|(&p, &ids)| LayeredGapTriangle {
                paired_vertex_ids: ids,
                start_m: p,
                end_m: p,
            })
            .collect();
        let mesh = if let Some(certificate) = closed_release {
            let (start, end) = match certificate {
                NativeClosedGeometrySelection::Derivative(certificate) => (
                    certificate.closed_start_charts().to_vec(),
                    certificate
                        .closed_start_charts()
                        .iter()
                        .copied()
                        .filter(|&chart| {
                            (0..3).all(|corner| {
                                certificate
                                    .closed_endpoint_corners()
                                    .contains(&(chart, corner))
                            })
                        })
                        .collect::<Vec<_>>(),
                ),
                NativeClosedGeometrySelection::Primal(witness) => (
                    witness.closed_start_charts().to_vec(),
                    witness.closed_end_charts().to_vec(),
                ),
                NativeClosedGeometrySelection::Sample(sample) => (
                    sample.closed_start_charts().to_vec(),
                    sample.closed_end_charts().to_vec(),
                ),
                NativeClosedGeometrySelection::Trial(sample) => (
                    sample.closed_start_charts().to_vec(),
                    sample.closed_end_charts().to_vec(),
                ),
            };
            gas_layered_tetra_mesh::build_closed_endpoint(
                &triangles,
                self.layers,
                S::one(),
                self.length_tolerance_m,
                &start,
                &end,
                fraction,
            )?
        } else {
            gas_layered_tetra_mesh::build(
                &triangles,
                self.layers,
                S::one(),
                self.length_tolerance_m,
            )?
        };
        let mut tets = vec![];
        for t in &mesh.tetrahedra {
            if t.volume_m3.value() == 0. {
                if t.volume_m3.has_direction() {
                    return Err(fail(
                        "closed native gas tetrahedron changes an active stratum",
                    ));
                }
                continue;
            }
            let region = (t.chart * self.layers + t.layer) * 3 + t.local_tetra;
            tets.push(CutTetraSubdomain {
                region,
                vertex_ids: Some(t.nodes),
                vertices_m: t.nodes.map(|i| mesh.nodes[i].end_m),
            });
        }
        Ok(tets)
    }
    fn endpoint_with_regions<S: TransientGasScalar>(
        &self,
        trace: &[S],
        design: &[S],
        tets: &[CutTetraSubdomain<S>],
    ) -> CaeResult<NativeGasGeometryEndpoint<S>> {
        Ok(self.endpoint_with_source_dispatch(trace,design,tets,false)?.0)
    }
    fn endpoint_with_source_dispatch<S: TransientGasScalar>(
        &self, trace: &[S], design: &[S], tets: &[CutTetraSubdomain<S>], record: bool,
    ) -> CaeResult<(NativeGasGeometryEndpoint<S>,Option<layered_aperture_dispatch::LayeredDispatch<S>>)> {
        let base = self.owner.partition(trace, design)?;
        let mut refined = native_cut_cell_refinement::refine(&base, &tets)?;
        self.canonicalize_source_facets(&mut refined,tets)?;
        let aperture_jets=AuthenticatedApertureJets::issue_owned(&refined,Some((tets,&self.owner.capacity_owner().grid)))?;
        let pending=if record{Some(layered_aperture_dispatch::PendingLayeredDispatch::issue(&refined,tets,&aperture_jets)?)}else{None};
        let (intersection_defects, wall_convex_support) =
            self.intersection_jets(&mut refined.partition, &tets, trace, design, &aperture_jets)?;
        self.hadamard_partition(
            &mut refined.partition,
            &refined.regions,
            &tets,
            trace,
            design,
        )?;
        let aggregated = self.layout.aggregate(&refined)?;
        let mut defects = BTreeMap::new();
        for c in &refined.partition.components {
            *defects.entry(c.cell).or_insert(S::zero()) += c.volume_m3;
        }
        for (&cell, &capacity) in &base.native_capacity_m3 {
            *defects.entry(cell).or_insert(S::zero()) -= capacity;
        }
        let mut partition_defects_m3: Vec<S> = self
            .owner
            .capacity_owner()
            .grid
            .cells
            .iter()
            .map(|cell| defects.get(cell).copied().unwrap_or(S::zero()))
            .collect();
        partition_defects_m3.extend(intersection_defects);
        let dispatch=pending.map(|p|p.bind(&refined,&aperture_jets)).transpose()?;
        Ok((NativeGasGeometryEndpoint {
            refined,
            aggregated,
            partition_defects_m3,
            wall_convex_support,
        },dispatch))
    }
    fn intersection_jets<S: TransientGasScalar>(
        &self,
        partition: &mut crate::native_cut_cell_partition::NativeCutCellPartition<S>,
        tets: &[CutTetraSubdomain<S>],
        trace: &[S],
        design: &[S],
        aperture_jets: &AuthenticatedApertureJets<S>,
    ) -> CaeResult<(Vec<S>, Vec<NativeGasConvexSupport<S>>)> {
        use crate::native_cut_cell_partition::{CutBoundary, area};
        let owner = self.owner.capacity_owner();
        let dot = |a: [S; 3], b: [S; 3]| (0..3).map(|k| a[k] * b[k]).fold(S::zero(), |a, b| a + b);
        let sub = |a: [S; 3], b: [S; 3]| std::array::from_fn(|k| a[k] - b[k]);
        let cross = |a: [S; 3], b: [S; 3]| {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let plane = |cell: usize,
                     boundary: &CutBoundary|
         -> CaeResult<([S; 3], [S; 3], S, CutBoundary)> {
            let (normal, origin, offset) = match boundary {
                CutBoundary::Cell { axis, side } => {
                    let b = owner.grid.cell_bounds(cell)?;
                    let n = std::array::from_fn(|k| S::from_f64(if k == *axis { 1. } else { 0. }));
                    (
                        n,
                        std::array::from_fn(|k| S::from_f64(b[*side][k])),
                        S::zero(),
                    )
                }
                CutBoundary::Subdomain { region, face } => {
                    let tet = tets
                        .iter()
                        .find(|t| t.region == *region)
                        .ok_or_else(|| fail("clipped vertex numerical plane is missing"))?;
                    let p = native_cut_cell_refinement::canonical_face(tet, *face)?;
                    (cross(sub(p[1], p[0]), sub(p[2], p[0])), p[0], S::zero())
                }
                CutBoundary::Native {
                    body,
                    element,
                    face,
                    ..
                } => {
                    let node = |i: usize| {
                        std::array::from_fn(|k| {
                            S::from_f64(owner.nodes[*body][i].reference_m[k])
                                + trace[owner.nodes[*body][i].force_flux[k]]
                        })
                    };
                    if let Some(f) = face {
                        let mut f = *f;
                        f.sort_unstable();
                        let p = f.map(node);
                        (cross(sub(p[1], p[0]), sub(p[2], p[0])), p[0], S::zero())
                    } else {
                        let ids = owner.elements[*body][*element];
                        let p = ids.map(node);
                        let offset = if *body == 0 { 0 } else { owner.nodes[0].len() };
                        let rho = ids.map(|i| {
                            let (c, v) = owner.phase.row(offset + i);
                            c.iter()
                                .zip(v)
                                .map(|(&j, &a)| design[j] * a)
                                .fold(S::zero(), |a, b| a + b)
                        });
                        let a = sub(p[1], p[0]);
                        let b = sub(p[2], p[0]);
                        let c = sub(p[3], p[0]);
                        let det = dot(a, cross(b, c));
                        let n = std::array::from_fn(|k| {
                            ((rho[1] - rho[0]) * cross(b, c)[k]
                                + (rho[2] - rho[0]) * cross(c, a)[k]
                                + (rho[3] - rho[0]) * cross(a, b)[k])
                                / det
                        });
                        (n, p[0], rho[0] - owner.iso)
                    }
                }
            };
            let axis = (0..3)
                .max_by(|&a, &b| normal[a].value().abs().total_cmp(&normal[b].value().abs()))
                .unwrap();
            let length = normal[axis];
            if !length.value().is_finite() || length.value() == 0. {
                return Err(fail("clipped vertex plane is degenerate"));
            }
            Ok((
                normal.map(|v| v / length),
                origin,
                offset / length,
                boundary.clone(),
            ))
        };
        let planes: Vec<Vec<([S; 3], [S; 3], S, CutBoundary)>> = partition
            .components
            .iter()
            .map(|c| {
                let mut boundaries = Vec::new();
                for p in c.convex_parts.iter().flat_map(|p| p.iter()) {
                    if !boundaries.contains(&p.boundary) {
                        boundaries.push(p.boundary.clone());
                    }
                }
                boundaries
                    .iter()
                    .map(|b| plane(c.cell, b))
                    .collect::<CaeResult<_>>()
            })
            .collect::<CaeResult<_>>()?;
        let part_planes = partition
            .components
            .iter()
            .map(|c| {
                c.convex_parts
                    .iter()
                    .map(|part| {
                        part.iter()
                            .map(|polygon| plane(c.cell, &polygon.boundary))
                            .collect::<CaeResult<Vec<_>>>()
                    })
                    .collect::<CaeResult<Vec<_>>>()
            })
            .collect::<CaeResult<Vec<_>>>()?;
        let producer_rounding_cache=std::cell::RefCell::new(None);
        let producer_bound=|component:usize,primary:Option<&[([S;3],[S;3],S,CutBoundary)]>,p:[S;3],candidate:[S;3],kind:&NativeCornerKind|->CaeResult<bool>{
            use crate::native_producer_rounding::ProducerRounded as R;
            let Some(primary)=primary else{return Ok(false)};
            let Some(part)=part_planes[component].iter().position(|actual|std::ptr::eq(actual.as_slice(),primary)) else{return Ok(false)};
            let NativeCornerKind::Edge{plane,..}=kind else{return Ok(false)};
            let actual=&partition.components[component].convex_parts[part];
            if !actual.iter().any(|face|&face.boundary==plane) || !actual.iter().flat_map(|face|&face.vertices_m).any(|point|(0..3).all(|k|aperture_leaf_bits(point[k],p[k]))){return Ok(false)}
            if producer_rounding_cache.borrow().is_none(){
                let replay_trace=trace.iter().map(|v|R::point(v.value())).collect::<Vec<_>>();
                let replay_design=design.iter().map(|v|R::point(v.value())).collect::<Vec<_>>();
                let replay_tets=tets.iter().map(|tet|CutTetraSubdomain{region:tet.region,vertex_ids:tet.vertex_ids,vertices_m:tet.vertices_m.map(|point|point.map(|v|R::point(v.value())))}).collect::<Vec<_>>();
                let replay_base=self.owner.partition(&replay_trace,&replay_design)?;
                *producer_rounding_cache.borrow_mut()=Some(native_cut_cell_refinement::refine(&replay_base,&replay_tets)?);
            }
            let cache=producer_rounding_cache.borrow();let replay=cache.as_ref().unwrap();
            let Some(replay_component)=replay.partition.components.get(component) else{return Ok(false)};
            let actual_component=&partition.components[component];
            if replay_component.cell!=actual_component.cell || replay_component.convex_parts.len()!=actual_component.convex_parts.len(){return Ok(false)}
            for (a,b) in actual_component.convex_parts.iter().zip(&replay_component.convex_parts){if a.len()!=b.len(){return Ok(false)}for (a,b) in a.iter().zip(b){if a.boundary!=b.boundary || a.support_only!=b.support_only || a.vertices_m.len()!=b.vertices_m.len() || a.vertices_m.iter().zip(&b.vertices_m).any(|(a,b)|(0..3).any(|k|a[k].value().to_bits()!=b[k].value().to_bits())){return Ok(false)}}}
            Ok(replay_component.convex_parts[part].iter().flat_map(|face|&face.vertices_m).any(|point|(0..3).all(|k|point[k].value().to_bits()==p[k].value().to_bits()&&point[k].contains(candidate[k].value()))))
        };
        use NativePointOwner as ParentOwner;
        let has_motion = |value: S| {
            value
                .geometry_first_order_directions()
                .is_none_or(|directions| directions.iter().any(|&direction| direction != 0.))
        };
        let mapped = self.kinematics.mapped_geometry_from_trace(trace, design, |v,_|Ok(v))?;
        let mut material_ids = BTreeMap::<(usize,usize),usize>::new();
        let mut material_points = Vec::<[S;3]>::new();
        let mut material_sources = BTreeMap::<usize,(usize,usize,usize)>::new();
        let mut native_material = BTreeMap::<usize,Vec<(usize,usize)>>::new();
        for (chart, points) in mapped.iter().enumerate() {
            let features = self.kinematics.chart_features(chart)?;
            for corner in 0..3 {
                let fixed = (0..3).all(|axis| {
                    let delta = points[corner+3][axis]-points[corner][axis];
                    delta.value()==0. && delta.maximum_direction()==0.
                });
                for layer in 0..=self.layers {
                    let fraction=layer as f64/self.layers as f64;
                    let point=std::array::from_fn(|axis|points[corner][axis]
                        +(points[corner+3][axis]-points[corner][axis])*fraction);
                    let key=(self.paired_vertex_ids[chart][corner],if fixed{0}else{layer});
                    let id=if let Some(&id)=material_ids.get(&key) {id} else {
                        let id=material_points.len();material_ids.insert(key,id);material_points.push(point);material_sources.insert(id,(chart,corner,layer));id
                    };
                    let body=if layer==0{Some(0)}else if layer==self.layers{Some(1)}else{None};
                    if let Some(body)=body {
                        if let crate::moving_contact::surface_map::SurfaceFeature::Vertex(node)=features[body][corner] {
                            let entry=native_material.entry(id).or_default();
                            if !entry.contains(&(body,node)){entry.push((body,node));}
                        }
                    }
                }
            }
        }
        for tet in tets {
            if let Some(ids)=tet.vertex_ids {
                for i in 0..4 {
                    let expected=material_points.get(ids[i])
                        .ok_or_else(||fail("layered native feature identity is absent"))?;
                    if (0..3).any(|k|expected[k].value().to_bits()!=tet.vertices_m[i][k].value().to_bits()
                        || has_motion(tet.vertices_m[i][k]-expected[k])) {
                        return Err(fail("layered native feature realization differs"));
                    }
                }
            }
        }
        let part_parents: Vec<Vec<Vec<([S; 3], ParentOwner)>>> = partition
            .components
            .iter()
            .map(|c| {
                c.convex_parts
                    .iter()
                    .map(|part| {
                        let mut points = Vec::new();
                        for polygon in part {
                            match polygon.boundary {
                                CutBoundary::Native { body, element, .. } => {
                                    for &node in &owner.elements[body][element] {
                                        points.push((
                                            std::array::from_fn(|k| {
                                                S::from_f64(owner.nodes[body][node].reference_m[k])
                                                    + trace[owner.nodes[body][node].force_flux[k]]
                                            }),
                                            ParentOwner::Native(body, node),
                                        ));
                                    }
                                }
                                CutBoundary::Subdomain { region, .. } => {
                                    if let Some(tet) = tets.iter().find(|t| t.region == region) {
                                        if let Some(ids) = tet.vertex_ids {
                                            points.extend((0..4).map(|i| {
                                                (tet.vertices_m[i], ParentOwner::Layered(ids[i]))
                                            }));
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        let mut seen = BTreeMap::<ParentOwner, [S; 3]>::new();
                        points.retain(|&(point, key)| {
                            if let Some(previous) = seen.get(&key) {
                                !(0..3).all(|k| {
                                    point[k].value().to_bits() == previous[k].value().to_bits()
                                        && point[k].geometry_first_order_directions()
                                            == previous[k].geometry_first_order_directions()
                                })
                            } else {
                                seen.insert(key, point);
                                true
                            }
                        });
                        points
                    })
                    .collect()
            })
            .collect();
        let zero_polygon = |points: &[[S; 3]]| -> CaeResult<bool> {
            let mut vector = [S::zero(); 3];
            for i in 1..points.len().saturating_sub(1) {
                let triangle = cross(sub(points[i], points[0]), sub(points[i + 1], points[0]));
                for k in 0..3 {
                    vector[k] += triangle[k] * 0.5;
                }
            }
            if vector.iter().any(|value| value.value() != 0.) {
                return Ok(false);
            }
            if vector.iter().any(|&value| has_motion(value)) {
                return Err(fail(
                    "zero-area authentic polygon has a nonzero geometric direction",
                ));
            }
            Ok(true)
        };
        let retained_vertices = |original: &[[S; 3]]| {
            let mut kept: Vec<usize> = (0..original.len()).collect();
            let norm = |v: [f64; 3]| v.iter().map(|x| x * x).sum::<f64>().sqrt();
            loop {
                if kept.len() <= 3 {
                    break;
                }
                let redundant = (0..kept.len()).find(|&i| {
                    let a = original[kept[(i + kept.len() - 1) % kept.len()]].map(|v| v.value());
                    let b = original[kept[i]].map(|v| v.value());
                    let c = original[kept[(i + 1) % kept.len()]].map(|v| v.value());
                    let edge = std::array::from_fn(|k| c[k] - a[k]);
                    let offset: [f64; 3] = std::array::from_fn(|k| b[k] - a[k]);
                    let length = norm(edge);
                    let t = (0..3).map(|k| edge[k] * offset[k]).sum::<f64>() / (length * length);
                    let area = norm([
                        offset[1] * edge[2] - offset[2] * edge[1],
                        offset[2] * edge[0] - offset[0] * edge[2],
                        offset[0] * edge[1] - offset[1] * edge[0],
                    ]);
                    length > partition.arithmetic_length_m
                        && t > 0.
                        && t < 1.
                        && area <= 8. * partition.arithmetic_length_m * length
                });
                if let Some(i) = redundant {
                    kept.remove(i);
                } else {
                    break;
                }
            }
            kept
        };
        let mut defects = Vec::new();
        let material_subdivision = std::cell::Cell::new(false);
        let last_construction = std::cell::RefCell::new(None);
        let mut vertex = |component: usize,
                          primary: Option<&[([S; 3], [S; 3], S, CutBoundary)]>,
                          p: [S; 3],
                          extra: &[([S; 3], [S; 3], S, CutBoundary)],
                          native_candidates: &[([S; 3], ParentOwner)],
                          edge_point: Option<[S; 3]>|
         -> CaeResult<[S; 3]> {
            let mut parent_candidates = native_candidates.to_vec();
            parent_candidates.sort_by_key(|candidate| candidate.1);
            for pair in parent_candidates.windows(2) {
                if pair[0].1 == pair[1].1
                    && (0..3).any(|k| {
                        has_motion(pair[0].0[k] - pair[1].0[k])
                            || pair[0].0[k].value().to_bits() != pair[1].0[k].value().to_bits()
                    })
                {
                    return Err(fail(
                        "genuine parent identity has incompatible realizations",
                    ));
                }
            }
            parent_candidates.dedup_by_key(|candidate| candidate.1);
            let native_candidates = parent_candidates.as_slice();
            let primal = p.map(|v| S::from_f64(v.value()));
            let owning = primary.unwrap_or(&planes[component]);
            let active: Vec<_> = owning
                .iter()
                .chain(extra.iter())
                .cloned()
                .filter(|(n, o, d, _)| {
                    ((0..3)
                        .map(|k| n[k].value() * (p[k].value() - o[k].value()))
                        .sum::<f64>()
                        + d.value())
                    .abs()
                        <= 8. * partition.arithmetic_length_m
                })
                .collect();
            *last_construction.borrow_mut() = None;
            let record = |actual: [S; 3], kind: NativeCornerKind| {
                let mut needed = std::collections::BTreeSet::new();
                let mut tet_vertices = BTreeMap::new();
                let mut add_parent = |owner: NativePointOwner| {
                    if let NativePointOwner::Layered(id) = owner {needed.insert(id);}
                };
                match &kind {
                    NativeCornerKind::Parent(owner)=>add_parent(*owner),
                    NativeCornerKind::Edge{left,right,..}=>{add_parent(*left);add_parent(*right);},
                    NativeCornerKind::PlaneIntersection{..}=>{},
                }
                for plane in &active {
                    if let CutBoundary::Subdomain{region,..}=plane.3 {
                        if let Some(ids)=tets.iter().find(|tet|tet.region==region).and_then(|tet|tet.vertex_ids) {
                            needed.extend(ids);tet_vertices.insert(region,ids);
                        }
                    }
                }
                let layered_sources = needed.into_iter().filter_map(|id|material_sources.get(&id)
                    .map(|&(chart,corner,layer)|(id,chart,corner,layer))).collect();
                *last_construction.borrow_mut() = Some(NativeCornerConstruction {
                    cell: partition.components[component].cell,
                    retained: p.map(|v| v.value()), actual,
                    active: active.iter().map(|p|p.3.clone()).collect(), kind, layered_sources, tet_vertices:tet_vertices.into_iter().collect(),
                });
            };
            let coordinate_owned = |mut point: [S; 3]| -> CaeResult<[S; 3]> {
                for axis in 0..3 {
                    let mut supported = active
                        .iter()
                        .filter(|plane| {
                            plane.0[axis].value() != 0.
                                && (0..3)
                                    .filter(|&k| k != axis)
                                    .all(|k| plane.0[k].value() == 0. && !has_motion(plane.0[k]))
                        })
                        .collect::<Vec<_>>();
                    supported.sort_by_key(|plane| match plane.3 {
                        CutBoundary::Native { .. } => 0,
                        CutBoundary::Cell { .. } => 1,
                        CutBoundary::Subdomain { .. } => 2,
                    });
                    if let Some(plane) = supported.first() {
                        let owned = plane.1[axis] - plane.2 / plane.0[axis];
                        if (owned.value() - point[axis].value()).abs()
                            > 8. * partition.arithmetic_length_m
                        {
                            return Err(fail(
                                "coordinate supporting owner exceeds representation bound",
                            ));
                        }
                        for plane in supported.iter().skip(1) {
                            let other = plane.1[axis] - plane.2 / plane.0[axis];
                            if has_motion(other - owned) {
                                return Err(fail(
                                    "coordinate supporting owners have incompatible motions",
                                ));
                            }
                        }
                        point[axis] = owned;
                    }
                }
                Ok(point)
            };
            if let Some(candidate) = edge_point {
                let candidate = coordinate_owned(candidate)?;
                for &(a, o, b, _) in &active {
                    let e = dot(a, sub(candidate, o)) + b;
                    defects.push((e - e.value()) * owner.grid.spacing_m.powi(2));
                }
                return Ok(candidate);
            }
            let exact_parents = native_candidates
                .iter()
                .filter(|&&(point, _)| {
                    (0..3).all(|k| point[k].value().to_bits() == p[k].value().to_bits())
                })
                .collect::<Vec<_>>();
            if let Some(&&(candidate, _)) = exact_parents.first() {
                if active.is_empty() {
                    return Err(fail("genuine retained parent has no supporting constraint"));
                }
                for &&(other, _) in &exact_parents {
                    if (0..3).any(|k| has_motion(other[k] - candidate[k])) {
                        return Err(fail("coincident native vertex motions are ambiguous"));
                    }
                }
                let candidate = coordinate_owned(candidate)?;
                for (a, o, b, _) in &active {
                    let residual = dot(*a, sub(candidate, *o)) + *b;
                    defects.push((residual - residual.value()) * owner.grid.spacing_m.powi(2));
                }
                record(candidate, NativeCornerKind::Parent(exact_parents[0].1));
                return Ok(candidate);
            }
            let own_count = owning
                .iter()
                .filter(|(n, o, d, _)| {
                    ((0..3)
                        .map(|k| n[k].value() * (p[k].value() - o[k].value()))
                        .sum::<f64>()
                        + d.value())
                    .abs()
                        <= 8. * partition.arithmetic_length_m
                })
                .count();
            let mut selected = None;
            let mut maximum = 0.;
            for count in [own_count, active.len()] {
                if maximum > 1024. * f64::EPSILON {
                    break;
                }
                for i in 0..count {
                    for j in i + 1..count {
                        for k in j + 1..count {
                            let a = active[i].0.map(|v| v.value());
                            let b = active[j].0.map(|v| v.value());
                            let c = active[k].0.map(|v| v.value());
                            let det = (a[0] * (b[1] * c[2] - b[2] * c[1])
                                + a[1] * (b[2] * c[0] - b[0] * c[2])
                                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                                .abs();
                            if det > maximum {
                                maximum = det;
                                selected = Some([i, j, k]);
                            }
                        }
                    }
                }
            }
            let supports = |plane: &([S; 3], [S; 3], S, CutBoundary), key: ParentOwner| match (
                &plane.3, key,
            ) {
                (
                    CutBoundary::Native {
                        body,
                        face: Some(face),
                        ..
                    },
                    ParentOwner::Native(parent_body, node),
                ) => *body == parent_body && face.contains(&node),
                (CutBoundary::Subdomain { region, face }, ParentOwner::Native(body,node)) => tets
                    .iter().find(|tet|tet.region==*region).and_then(|tet|tet.vertex_ids)
                    .is_some_and(|ids|[[0,1,2],[0,1,3],[0,2,3],[1,2,3]][*face].iter()
                        .any(|&i|native_material.get(&ids[i]).is_some_and(|owners|owners.contains(&(body,node))))),
                (CutBoundary::Native{body,face:Some(face),..}, ParentOwner::Layered(vertex)) =>
                    native_material.get(&vertex).is_some_and(|owners|owners.iter()
                        .any(|&(parent_body,node)|parent_body==*body && face.contains(&node))),
                (CutBoundary::Subdomain { region, face }, ParentOwner::Layered(vertex)) => tets
                    .iter()
                    .find(|tet| tet.region == *region)
                    .and_then(|tet| tet.vertex_ids)
                    .is_some_and(|ids| {
                        [[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]][*face]
                            .iter()
                            .any(|&i| ids[i] == vertex)
                    }),
                _ => false,
            };
            let parent_supported = |key: ParentOwner| -> bool {
                let supported = active
                    .iter()
                    .filter(|plane| supports(plane, key))
                    .collect::<Vec<_>>();
                (0..supported.len()).any(|i| {
                    (i + 1..supported.len()).any(|j| {
                        (j + 1..supported.len()).any(|k| {
                            dot(supported[i].0, cross(supported[j].0, supported[k].0))
                                .value()
                                .abs()
                                > 1024. * f64::EPSILON
                        })
                    })
                })
            };
            let authenticated = native_candidates
                .iter()
                .filter(|&&(point, key)| {
                    (0..3).all(|k| point[k].value().to_bits() == p[k].value().to_bits())
                        || parent_supported(key)
                })
                .collect::<Vec<_>>();
            if let Some(&&(candidate, _)) = authenticated.first() {
                for &&(other, _) in &authenticated {
                    if (0..3).any(|k| {
                        has_motion(other[k] - candidate[k])
                            || other[k].value().to_bits() != candidate[k].value().to_bits()
                    }) {
                        return Err(fail("coincident native vertex motions are ambiguous"));
                    }
                }
                let candidate = coordinate_owned(candidate)?;
                if (0..3).any(|k| {
                    (candidate[k].value() - p[k].value()).abs() > 8. * partition.arithmetic_length_m
                }) {
                    return Err(fail(
                        "topological parent exceeds arithmetic representation bound",
                    ));
                }
                for (a, o, b, _) in &active {
                    let residual = dot(*a, sub(candidate, *o)) + *b;
                    defects.push((residual - residual.value()) * owner.grid.spacing_m.powi(2));
                }
                record(candidate, NativeCornerKind::Parent(authenticated[0].1));
                return Ok(candidate);
            }
            let mut edge_parents = Vec::new();
            for i in 0..native_candidates.len() {
                for j in i + 1..native_candidates.len() {
                    let (left, left_key) = native_candidates[i];
                    let (right, right_key) = native_candidates[j];
                    let same_family = match (left_key, right_key) {
                        (ParentOwner::Native(a, i), ParentOwner::Native(b, j)) => a == b && i != j,
                        (ParentOwner::Layered(i), ParentOwner::Layered(j)) => i != j,
                        _ => false,
                    };
                    if !same_family {
                        continue;
                    }
                    let support = active
                        .iter()
                        .filter(|plane| supports(plane, left_key) && supports(plane, right_key))
                        .collect::<Vec<_>>();
                    if !(0..support.len()).any(|i| {
                        (i + 1..support.len()).any(|j| {
                            dot(
                                cross(support[i].0, support[j].0),
                                cross(support[i].0, support[j].0),
                            )
                            .value()
                                > 1024. * f64::EPSILON
                        })
                    }) {
                        continue;
                    }
                    let edge = sub(right, left);
                    let edge_length = dot(edge, edge).value().sqrt();
                    let t = (0..3)
                        .map(|k| (p[k].value() - left[k].value()) * edge[k].value())
                        .sum::<f64>()
                        / edge_length.powi(2);
                    if !t.is_finite()
                        || t < 0.
                        || t > 1.
                        || (0..3).any(|k| {
                            (left[k].value() + t * edge[k].value() - p[k].value()).abs()
                                > 8. * partition.arithmetic_length_m
                        })
                    {
                        continue;
                    }
                    let plane = active
                        .iter()
                        .max_by(|a, b| {
                            dot(a.0, edge)
                                .value()
                                .abs()
                                .total_cmp(&dot(b.0, edge).value().abs())
                        })
                        .unwrap();
                    let denominator = dot(plane.0, edge);
                    let parameter =
                        if denominator.value().abs() <= 1024. * f64::EPSILON * edge_length {
                            material_subdivision.set(true);
                            S::from_f64(t)
                        } else {
                            (-dot(plane.0, sub(left, plane.1)) - plane.2) / denominator
                        };
                    let candidate: [S; 3] = std::array::from_fn(|k| left[k] + parameter * edge[k]);
                    edge_parents.push((candidate, NativeCornerKind::Edge { left: left_key, right: right_key, plane: plane.3.clone() }));
                }
            }
            if let Some((candidate, kind)) = edge_parents.first() {
                let candidate = *candidate;
                let candidate = coordinate_owned(candidate)?;
                if (0..3).any(|k| {
                    (candidate[k].value() - p[k].value()).abs() > 8. * partition.arithmetic_length_m
                }) && !producer_bound(component, primary, p, candidate, kind)? {
                    return Err(fail(
                        "topological edge exceeds arithmetic representation bound",
                    ));
                }
                for (other, other_kind) in &edge_parents {
                    let other = coordinate_owned(*other)?;
                    if (0..3).any(|k| {
                        (other[k].value() - p[k].value()).abs() > 8. * partition.arithmetic_length_m
                    }) && !producer_bound(component, primary, p, other, other_kind)? {
                        return Err(fail(
                            "topological edge exceeds arithmetic representation bound",
                        ));
                    }
                    for (a, o, b, _) in &active {
                        let residual = dot(*a, sub(other, *o)) + *b;
                        defects.push((residual - residual.value()) * owner.grid.spacing_m.powi(2));
                    }
                }
                for (a, o, b, _) in &active {
                    let residual = dot(*a, sub(candidate, *o)) + *b;
                    defects.push((residual - residual.value()) * owner.grid.spacing_m.powi(2));
                }
                record(candidate, kind.clone());
                return Ok(candidate);
            }
            let ids = selected
                    .ok_or_else(|| fail(&format!("clipped vertex has no independent plane intersection in cell {} at {:?} with {} incident planes", partition.components[component].cell, p.map(|v| v.value()), active.len())))?;
            if maximum <= 1024. * f64::EPSILON {
                return Err(fail(&format!(
                    "clipped vertex plane intersection is ill conditioned in cell {} at {:?}, determinant {:e}",
                    partition.components[component].cell,
                    p.map(|v| v.value()),
                    maximum
                )));
            }
            let n = ids.map(|i| active[i].0.map(|v| S::from_f64(v.value())));
            let determinant = dot(n[0], cross(n[1], n[2]));
            let anchor_id = *ids
                .iter()
                .min_by(|&&i, &&j| {
                    let distance = |id: usize| {
                        (0..3)
                            .map(|k| (active[id].1[k].value() - primal[k].value()).abs())
                            .fold(0., f64::max)
                    };
                    distance(i).total_cmp(&distance(j))
                })
                .unwrap();
            let anchor = active[anchor_id].1;
            let anchor_value = anchor.map(|v| S::from_f64(v.value()));
            let rhs = ids.map(|i| {
                S::from_f64(
                    (-dot(active[i].0, sub(anchor_value, active[i].1)) - active[i].2).value(),
                )
            });
            let delta: [S; 3] = std::array::from_fn(|k| {
                (cross(n[1], n[2])[k] * rhs[0]
                    + cross(n[2], n[0])[k] * rhs[1]
                    + cross(n[0], n[1])[k] * rhs[2])
                    / determinant
            });
            let canonical: [S; 3] = std::array::from_fn(|k| anchor_value[k] + delta[k]);
            if (0..3).any(|k| {
                (canonical[k].value() - primal[k].value()).abs()
                    > 8. * partition.arithmetic_length_m
            }) {
                return Err(fail(
                    "canonical aperture vertex exceeds arithmetic representation bound",
                ));
            }
            let origin_relative = std::array::from_fn(|k| anchor[k] + delta[k]);
            let r = ids.map(|i| {
                let e = &active[i];
                let residual = -dot(e.0, sub(origin_relative, e.1)) - e.2;
                residual - residual.value()
            });
            let moving_n = ids.map(|i| active[i].0);
            let moving_det = dot(moving_n[0], cross(moving_n[1], moving_n[2]));
            let corrected = std::array::from_fn(|k| {
                origin_relative[k]
                    + (cross(moving_n[1], moving_n[2])[k] * r[0]
                        + cross(moving_n[2], moving_n[0])[k] * r[1]
                        + cross(moving_n[0], moving_n[1])[k] * r[2])
                        / moving_det
            });
            let corrected = coordinate_owned(corrected)?;
            for (index, &(a, o, b, _)) in active.iter().enumerate() {
                if ids.contains(&index) {
                    continue;
                }
                let residual = dot(a, sub(corrected, o)) + b;
                defects.push((residual - residual.value()) * owner.grid.spacing_m.powi(2));
            }
            record(corrected, NativeCornerKind::PlaneIntersection {
                selected: ids.map(|i|active[i].3.clone()),
                anchor: active[anchor_id].3.clone(), delta: delta.map(|v|v.value()),
            });
            Ok(corrected)
        };
        let mut wall_constructions = Vec::with_capacity(partition.walls.len());
        for wall in &mut partition.walls {
            material_subdivision.set(false);
            let ids = wall.native_nodes;
            let mut extra = [[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]]
                .iter()
                .map(|f| {
                    plane(
                        partition.components[wall.component].cell,
                        &CutBoundary::Native {
                            body: wall.body,
                            element: wall.element,
                            face: Some(f.map(|j| ids[j])),
                            physical: false,
                        },
                    )
                })
                .collect::<CaeResult<Vec<_>>>()?;
            let phase_offset = if wall.body == 0 {
                0
            } else {
                owner.nodes[0].len()
            };
            let rho = ids.map(|id| {
                let (columns, weights) = owner.phase.row(phase_offset + id);
                columns
                    .iter()
                    .zip(weights)
                    .map(|(&column, &weight)| design[column] * weight)
                    .fold(S::zero(), |a, b| a + b)
            });
            if rho.iter().any(|value| value.value() != rho[0].value()) {
                extra.push(plane(
                    partition.components[wall.component].cell,
                    &CutBoundary::Native {
                        body: wall.body,
                        element: wall.element,
                        face: None,
                        physical: true,
                    },
                )?);
            }
            let primary = part_planes[wall.component]
                .get(wall.convex_part)
                .ok_or_else(|| fail("native wall convex owner missing"))?;
            let original = wall.vertices_m.clone();
            let kept = retained_vertices(&original);
            let mut constructions = Vec::with_capacity(kept.len());
            wall.vertices_m = kept
                .iter()
                .map(|&i| {
                    let actual = vertex(
                        wall.component,
                        Some(primary),
                        original[i],
                        &extra,
                        &part_parents[wall.component][wall.convex_part],
                        None,
                    )?;
                    constructions.push(last_construction.borrow().clone()
                        .ok_or_else(||fail("authentic wall corner construction is absent"))?);
                    Ok(actual)
                })
                .collect::<CaeResult<_>>()?;
            wall_constructions.push(constructions);
            for i in 0..original.len() {
                if kept.contains(&i) {
                    continue;
                }
                let right = kept.iter().position(|&j| j > i).unwrap_or(0);
                let left = (right + kept.len() - 1) % kept.len();
                let a = original[kept[left]].map(|v| v.value());
                let b = original[kept[right]].map(|v| v.value());
                let edge: [f64; 3] = std::array::from_fn(|k| b[k] - a[k]);
                let t = (0..3)
                    .map(|k| (original[i][k].value() - a[k]) * edge[k])
                    .sum::<f64>()
                    / edge.iter().map(|v| v * v).sum::<f64>();
                let candidate = std::array::from_fn(|k| {
                    wall.vertices_m[left][k] * (1. - t) + wall.vertices_m[right][k] * t
                });
                vertex(
                    wall.component,
                    Some(primary),
                    original[i],
                    &extra,
                    &part_parents[wall.component][wall.convex_part],
                    Some(candidate),
                )?;
            }
            let collapsed = zero_polygon(&wall.vertices_m)?;
            if material_subdivision.get() && !collapsed {
                return Err(fail(
                    "rank-two clipping subdivision has no positive-face material owner",
                ));
            }
            wall.area_m2 = if collapsed {
                S::zero()
            } else {
                area(&wall.vertices_m)
            };
            let nodes = wall.native_nodes.map(|i| {
                std::array::from_fn(|k| {
                    S::from_f64(owner.nodes[wall.body][i].reference_m[k])
                        + trace[owner.nodes[wall.body][i].force_flux[k]]
                })
            });
            wall.native_barycentric = wall
                .vertices_m
                .iter()
                .map(|&p| material_barycentric(p, nodes))
                .collect::<CaeResult<_>>()?;
        }
        for (aperture_index, aperture) in partition.apertures.iter_mut().enumerate() {
            if aperture_jets.validate(aperture_index,aperture)? { continue; }
            if aperture.polygon_parts.len() != aperture.polygons.len() {
                return Err(fail("aperture convex provenance shape"));
            }
            for (polygon, &(left_part, right_part)) in
                aperture.polygons.iter_mut().zip(&aperture.polygon_parts)
            {
                material_subdivision.set(false);
                let primary = part_planes[aperture.left]
                    .get(left_part)
                    .ok_or_else(|| fail("aperture left convex owner absent"))?;
                let extra = match (aperture.right, right_part) {
                    (Some(right), Some(part)) => part_planes[right]
                        .get(part)
                        .ok_or_else(|| fail("aperture right convex owner absent"))?
                        .as_slice(),
                    (None, None) => &[],
                    _ => return Err(fail("aperture convex owner mismatch")),
                };
                let mut aperture_parents = part_parents[aperture.left][left_part].clone();
                if let (Some(right), Some(part)) = (aperture.right, right_part) {
                    aperture_parents.extend_from_slice(&part_parents[right][part]);
                }
                let original = polygon.clone();
                let kept = retained_vertices(&original);
                *polygon = kept
                    .iter()
                    .map(|&i| {
                        vertex(
                            aperture.left,
                            Some(primary),
                            original[i],
                            extra,
                            &aperture_parents,
                            None,
                        )
                    })
                    .collect::<CaeResult<_>>()?;
                for i in 0..original.len() {
                    if kept.contains(&i) {
                        continue;
                    }
                    let right = kept.iter().position(|&j| j > i).unwrap_or(0);
                    let left = (right + kept.len() - 1) % kept.len();
                    let a = original[kept[left]].map(|v| v.value());
                    let b = original[kept[right]].map(|v| v.value());
                    let e: [f64; 3] = std::array::from_fn(|k| b[k] - a[k]);
                    let t = (0..3)
                        .map(|k| (original[i][k].value() - a[k]) * e[k])
                        .sum::<f64>()
                        / e.iter().map(|v| v * v).sum::<f64>();
                    let candidate = std::array::from_fn(|k| {
                        polygon[left][k] * (1. - t) + polygon[right][k] * t
                    });
                    vertex(
                        aperture.left,
                        Some(primary),
                        original[i],
                        extra,
                        &aperture_parents,
                        Some(candidate),
                    )?;
                }
                if material_subdivision.get() && !zero_polygon(polygon)? {
                    return Err(fail(
                        "rank-two clipping subdivision has no positive-face material owner",
                    ));
                }
            }
            let mut polygons = Vec::new();
            let mut owners = Vec::new();
            for (polygon, owner) in std::mem::take(&mut aperture.polygons)
                .into_iter()
                .zip(std::mem::take(&mut aperture.polygon_parts))
            {
                if !zero_polygon(&polygon)? {
                    polygons.push(polygon);
                    owners.push(owner);
                }
            }
            aperture.polygons = polygons;
            aperture.polygon_parts = owners;
            aperture.area_m2 = aperture
                .polygons
                .iter()
                .map(|p| area(p))
                .fold(S::zero(), |a, b| a + b);
        }
        let mut retained_wall_constructions = Vec::new();
        partition.walls = std::mem::take(&mut partition.walls).into_iter()
            .zip(wall_constructions).filter_map(|(wall, constructions)| {
                if wall.area_m2.value() != 0. {
                    retained_wall_constructions.push(constructions);
                    Some(wall)
                } else { None }
            }).collect();
        partition
            .apertures
            .retain(|aperture| !aperture.polygons.is_empty());
        let mut supports = BTreeMap::new();
        for wall in &partition.walls {
            let key = (wall.component, wall.convex_part);
            if supports.contains_key(&key) {
                continue;
            }
            let part = &partition.components[wall.component].convex_parts[wall.convex_part];
            let primary = &part_planes[wall.component][wall.convex_part];
            let mut polygons = Vec::with_capacity(part.len());
            let mut constructions = Vec::with_capacity(part.len());
            for polygon in part {
                material_subdivision.set(false);
                let mut live = polygon.clone();
                let mut corner_constructions = Vec::with_capacity(polygon.vertices_m.len());
                live.vertices_m = polygon.vertices_m.iter().map(|&point| {
                    let actual = vertex(wall.component, Some(primary), point, &[],
                        &part_parents[wall.component][wall.convex_part], None)?;
                    let construction = last_construction.borrow().clone()
                        .ok_or_else(||fail("authentic convex support corner construction is absent"))?;
                    corner_constructions.push(construction);
                    Ok(actual)
                }).collect::<CaeResult<Vec<_>>>()?;
                constructions.push(corner_constructions);
                let (normal, origin, offset, _) = plane(
                    partition.components[wall.component].cell, &polygon.boundary)?;
                let orientation = dot(normal, polygon.outward_normal).value();
                if !orientation.is_finite() || orientation == 0. {
                    return Err(fail("convex support plane orientation is unresolved"));
                }
                let scale = S::from_f64(if orientation > 0. { 1. } else { -1. });
                live.outward_normal = normal.map(|v| v * scale);
                live.plane_origin_m = origin;
                live.plane_offset_m = offset * scale;
                polygons.push(live);
            }
            supports.insert(key, (polygons, constructions));
        }
        let wall_convex_support = partition.walls.iter().enumerate().map(|(wall_index, wall)| {
            Ok(NativeGasConvexSupport {
                component: wall.component,
                convex_part: wall.convex_part,
                plane_tet_vertices: supports.get(&(wall.component, wall.convex_part)).ok_or_else(||fail("native wall support is absent"))?.0.iter().filter_map(|p|match p.boundary {crate::native_cut_cell_partition::CutBoundary::Subdomain{region,..}=>Some(region),_=>None}).map(|region| {
                    let ids=tets.iter().find(|t|t.region==region).and_then(|t|t.vertex_ids).ok_or_else(||fail("native support plane source tetrahedron is absent"))?;
                    Ok((region,ids))
                }).collect::<CaeResult<Vec<_>>>()?,
                plane_layered_sources: supports.get(&(wall.component, wall.convex_part)).ok_or_else(||fail("native wall support is absent"))?.0.iter().filter_map(|p|match p.boundary {crate::native_cut_cell_partition::CutBoundary::Subdomain{region,..}=>Some(region),_=>None}).flat_map(|region|tets.iter().find(|t|t.region==region).and_then(|t|t.vertex_ids).into_iter().flatten()).map(|id| {
                    let &(chart,corner,layer)=material_sources.get(&id).ok_or_else(||fail("native support plane layered feature is absent"))?;
                    Ok((id,chart,corner,layer))
                }).collect::<CaeResult<Vec<_>>>()?,
                wall_constructions: retained_wall_constructions.get(wall_index)
                    .ok_or_else(||fail("native wall corner owner is absent"))?.clone(),
                polygons: supports.get(&(wall.component, wall.convex_part))
                    .ok_or_else(|| fail("native wall convex support is absent"))?.0.clone(),
                corner_constructions: supports.get(&(wall.component, wall.convex_part))
                    .ok_or_else(|| fail("native wall convex corner construction is absent"))?.1.clone(),
            })
        }).collect::<CaeResult<Vec<_>>>()?;
        Ok((defects, wall_convex_support))
    }
    fn hadamard_partition<S: TransientGasScalar>(
        &self,
        partition: &mut crate::native_cut_cell_partition::NativeCutCellPartition<S>,
        regions: &[Option<usize>],
        tets: &[CutTetraSubdomain<S>],
        trace: &[S],
        design: &[S],
    ) -> CaeResult<()> {
        let owner = self.owner.capacity_owner();
        let mut volume = partition
            .components
            .iter()
            .map(|c| S::from_f64(c.volume_m3.value()))
            .collect::<Vec<_>>();
        let mut moment = partition
            .components
            .iter()
            .map(|c| {
                c.centroid_m
                    .map(|p| S::from_f64(p.value() * c.volume_m3.value()))
            })
            .collect::<Vec<_>>();
        let anchors = partition
            .components
            .iter()
            .map(|c| c.centroid_m.map(|v| v.value()))
            .collect::<Vec<_>>();
        let mut second = partition
            .components
            .iter()
            .map(|c| {
                c.central_second_integral_m5
                    .map(|row| row.map(|v| S::from_f64(v.value())))
            })
            .collect::<Vec<_>>();
        let dot = |a: [f64; 3], b: [f64; 3]| (0..3).map(|k| a[k] * b[k]).sum::<f64>();
        let sub = |a: [f64; 3], b: [f64; 3]| std::array::from_fn(|k| a[k] - b[k]);
        let cross = |a: [f64; 3], b: [f64; 3]| {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let bary = |p: [f64; 3], v: [[f64; 3]; 4]| -> CaeResult<[f64; 4]> {
            let a = sub(v[1], v[0]);
            let b = sub(v[2], v[0]);
            let c = sub(v[3], v[0]);
            let d = sub(p, v[0]);
            let det = dot(a, cross(b, c));
            if !det.is_finite() || det == 0. {
                return Err(fail("invalid native gas material chart"));
            }
            let w = [
                dot(d, cross(b, c)) / det,
                dot(a, cross(d, c)) / det,
                dot(a, cross(b, d)) / det,
            ];
            Ok([1. - w.iter().sum::<f64>(), w[0], w[1], w[2]])
        };
        let mut add = |component: usize,
                       p: [f64; 3],
                       normal: [f64; 3],
                       area: f64,
                       point: [S; 3],
                       sign: f64| {
            let dv = (0..3)
                .map(|k| (point[k] - point[k].value()) * (normal[k] * area * sign))
                .fold(S::zero(), |a, b| a + b);
            volume[component] += dv;
            for k in 0..3 {
                moment[component][k] += dv * p[k];
            }
            for j in 0..3 {
                for k in 0..3 {
                    second[component][j][k] +=
                        dv * (p[j] - anchors[component][j]) * (p[k] - anchors[component][k]);
                }
            }
        };
        for q in partition.wall_moment_quadrature() {
            let nodes = &owner.nodes[q.body];
            let weights = q.native_barycentric.map(|v| v.value());
            let physical: [[S; 3]; 4] = q.native_nodes.map(|i| {
                std::array::from_fn(|k| {
                    S::from_f64(nodes[i].reference_m[k]) + trace[nodes[i].force_flux[k]]
                })
            });
            let mut point: [S; 3] = std::array::from_fn(|k| {
                (0..4)
                    .map(|i| physical[i][k] * weights[i])
                    .fold(S::zero(), |a, b| a + b)
            });
            let offset = if q.body == 0 { 0 } else { owner.nodes[0].len() };
            let density: [S; 4] = q.native_nodes.map(|i| {
                let (c, v) = owner.phase.row(offset + i);
                c.iter()
                    .zip(v)
                    .map(|(&j, &a)| design[j] * a)
                    .fold(S::zero(), |a, b| a + b)
            });
            let rho = (0..4).map(|i| density[i].value() * weights[i]).sum::<f64>();
            let rho_scale = density
                .iter()
                .map(|v| v.value().abs())
                .fold(owner.iso.abs(), f64::max);
            if (rho - owner.iso).abs() <= 512. * f64::EPSILON * rho_scale.max(f64::MIN_POSITIVE) {
                let p = physical.map(|p| p.map(|v| v.value()));
                let a = sub(p[1], p[0]);
                let b = sub(p[2], p[0]);
                let c = sub(p[3], p[0]);
                let det = dot(a, cross(b, c));
                let gradient = std::array::from_fn(|k| {
                    ((density[1].value() - density[0].value()) * cross(b, c)[k]
                        + (density[2].value() - density[0].value()) * cross(c, a)[k]
                        + (density[3].value() - density[0].value()) * cross(a, b)[k])
                        / det
                });
                let normal = q.normal.map(|v| v.value());
                let denominator = dot(gradient, normal);
                if !denominator.is_finite() || denominator == 0. {
                    return Err(fail(
                        "native gas phase boundary has no calibrated normal gradient",
                    ));
                }
                let shift = (0..4)
                    .map(|i| (density[i] - density[i].value()) * weights[i])
                    .fold(S::zero(), |a, b| a + b)
                    / denominator;
                for k in 0..3 {
                    point[k] -= shift * normal[k];
                }
            }
            add(
                q.component,
                q.position_m.map(|v| v.value()),
                q.normal.map(|v| v.value()),
                q.area_m2.value(),
                point,
                1.,
            );
        }
        for q in partition.aperture_moment_quadrature() {
            let aperture = &partition.apertures[q.aperture];
            if aperture.axis.is_some() {
                continue;
            }
            let region = regions[q.left]
                .or_else(|| q.right.and_then(|r| regions[r]))
                .ok_or_else(|| fail("moving native gas aperture has no material chart"))?;
            let tet = tets
                .iter()
                .find(|t| t.region == region)
                .ok_or_else(|| fail("native gas aperture chart is missing"))?;
            let p = q.position_m.map(|v| v.value());
            let w = bary(p, tet.vertices_m.map(|v| v.map(|x| x.value())))?;
            let point = std::array::from_fn(|k| {
                (0..4)
                    .map(|i| tet.vertices_m[i][k] * w[i])
                    .fold(S::zero(), |a, b| a + b)
            });
            let normal = q.normal.map(|v| v.value());
            let area = q.area_m2.value();
            add(q.left, p, normal, area, point, 1.);
            if let Some(r) = q.right {
                add(r, p, normal, area, point, -1.);
            }
        }
        for (((component, v), m), c) in partition
            .components
            .iter_mut()
            .zip(volume)
            .zip(moment)
            .zip(second)
        {
            component.volume_m3 = v;
            component.centroid_m = m.map(|x| x / v);
            component.central_second_integral_m5 = c;
        }
        partition.total_volume_m3 = partition
            .components
            .iter()
            .map(|c| c.volume_m3)
            .fold(S::zero(), |a, b| a + b);
        Ok(())
    }
    pub fn initial_inventory<S: GeometryTimeBase>(
        &self,
        trace: &[S],
        design: &[S],
        density_kg_m3: S,
        velocity_m_s: [S; 3],
        gas_rt_m2_s2: S,
        grid: &TransientVelocityGrid,
        policy: &TransientGasPolicy,
    ) -> CaeResult<Vec<S>> {
        let geometry = self.endpoint(trace, design)?;
        let density =
            velocity_equilibrium_density(grid, density_kg_m3, velocity_m_s, gas_rt_m2_s2, policy)?;
        Ok(geometry
            .aggregated
            .volumes_m3
            .iter()
            .flat_map(|&volume| density.iter().map(move |&q| q * volume))
            .collect())
    }
    pub fn project_closed_geometry_trace<S: GeometryTimeBase>(
        &self,
        start: &[S],
        end: &[S],
        design: &[S],
        time_scale: S,
        certificate: &crate::native_closed_release::NativeClosedReleaseCertificate,
    ) -> CaeResult<(Vec<S>, Vec<S>)> {
        self.project_closed_geometry_trace_owned(start, end, design, time_scale, certificate)
    }
    fn project_closed_geometry_trace_owned<S: GeometryTimeBase>(
        &self,
        start: &[S],
        end: &[S],
        design: &[S],
        time_scale: S,
        certificate: &dyn crate::native_closed_release::NativeClosedGeometryProjector,
    ) -> CaeResult<(Vec<S>, Vec<S>)> {
        certificate.validate_time_scale(time_scale.value())?;
        certificate.validate_path(
            &self.kinematics,
            &start.iter().map(|v| v.value()).collect::<Vec<_>>(),
            &end.iter().map(|v| v.value()).collect::<Vec<_>>(),
            &design.iter().map(|v| v.value()).collect::<Vec<_>>(),
        )?;
        let raw: Vec<_> = start
            .iter()
            .chain(end)
            .chain(design)
            .copied()
            .chain(std::iter::once(time_scale))
            .collect();
        let active: Vec<_> = raw
            .iter()
            .enumerate()
            .filter_map(|(i, value)| value.has_direction().then_some(i))
            .collect();
        let inputs: Vec<_> = active.iter().map(|&i| raw[i]).collect();
        let mut columns = Vec::with_capacity(active.len());
        for &index in &active {
            let mut a = vec![0.; start.len()];
            let mut b = vec![0.; end.len()];
            let mut x = vec![0.; design.len()];
            let mut time = 0.;
            if index < a.len() {
                a[index] = 1.;
            } else if index < a.len() + b.len() {
                b[index - a.len()] = 1.;
            } else if index < a.len() + b.len() + x.len() {
                x[index - a.len() - b.len()] = 1.;
            } else {
                time = 1.;
            }
            columns.push(certificate.project_direction(&a, &b, Some(&x), time)?);
        }
        let a = start
            .iter()
            .enumerate()
            .map(|(i, value)| {
                S::lift_geometry_partial(
                    value.value(),
                    &inputs,
                    &columns.iter().map(|column| column.0[i]).collect::<Vec<_>>(),
                )
            })
            .collect::<CaeResult<Vec<_>>>()?;
        let b = end
            .iter()
            .enumerate()
            .map(|(i, value)| {
                S::lift_geometry_partial(
                    value.value(),
                    &inputs,
                    &columns.iter().map(|column| column.1[i]).collect::<Vec<_>>(),
                )
            })
            .collect::<CaeResult<Vec<_>>>()?;
        Ok((a, b))
    }
    pub fn initial_inventory_with_closed_release<S: GeometryTimeBase>(
        &self,
        start: &[S],
        end: &[S],
        design: &[S],
        density_kg_m3: S,
        velocity_m_s: [S; 3],
        gas_rt_m2_s2: S,
        grid: &TransientVelocityGrid,
        policy: &TransientGasPolicy,
        certificate: &crate::native_closed_release::NativeClosedReleaseCertificate,
    ) -> CaeResult<Vec<S>> {
        certificate.validate_path(
            &self.kinematics,
            &start.iter().map(|v| v.value()).collect::<Vec<_>>(),
            &end.iter().map(|v| v.value()).collect::<Vec<_>>(),
            &design.iter().map(|v| v.value()).collect::<Vec<_>>(),
        )?;
        for k in 0..S::direction_count() {
            certificate.validate_direction(
                &start.iter().map(|v| v.direction_at(k)).collect::<Vec<_>>(),
                &end.iter().map(|v| v.direction_at(k)).collect::<Vec<_>>(),
                Some(&design.iter().map(|v| v.direction_at(k)).collect::<Vec<_>>()),
                0.,
            )?;
        }
        let tets = self.chart_subdomains_selected(
            start,
            design,
            Some(NativeClosedGeometrySelection::Derivative(certificate)),
            0.,
        )?;
        let geometry = self.endpoint_with_regions(start, design, &tets)?;
        let density =
            velocity_equilibrium_density(grid, density_kg_m3, velocity_m_s, gas_rt_m2_s2, policy)?;
        Ok(geometry
            .aggregated
            .volumes_m3
            .iter()
            .flat_map(|&volume| density.iter().map(move |&q| q * volume))
            .collect())
    }
    pub fn initial_inventory_with_closed_witness(
        &self,
        start: &[f64],
        end: &[f64],
        design: &[f64],
        density_kg_m3: f64,
        velocity_m_s: [f64; 3],
        gas_rt_m2_s2: f64,
        grid: &TransientVelocityGrid,
        policy: &TransientGasPolicy,
        witness: &crate::native_closed_release::NativeClosedGeometryWitness,
    ) -> CaeResult<Vec<f64>> {
        witness.validate_path(&self.kinematics, start, end, design)?;
        let tets = self.chart_subdomains_selected(
            start,
            design,
            Some(NativeClosedGeometrySelection::Primal(witness)),
            0.,
        )?;
        let geometry = self.endpoint_with_regions(start, design, &tets)?;
        let density =
            velocity_equilibrium_density(grid, density_kg_m3, velocity_m_s, gas_rt_m2_s2, policy)?;
        Ok(geometry
            .aggregated
            .volumes_m3
            .iter()
            .flat_map(|&volume| density.iter().map(move |&q| q * volume))
            .collect())
    }
    pub fn assemble<S: GeometryTimeBase>(
        &self,
        start: &[S],
        end: &[S],
        design: &[S],
        step_s: S,
        inventory: &[S],
        ports: &BTreeMap<(Option<usize>, usize, usize), usize>,
        port_density_kg_m3: Vec<Vec<S>>,
        grid: TransientVelocityGrid,
        gas_rt_m2_s2: S,
        viscosity_pa_s: S,
        policy: &TransientGasPolicy,
    ) -> CaeResult<NativeGasTransportAssembly<S>> {
        let boundaries = ports
            .iter()
            .map(|(&k, &port)| (k, NativeGasExteriorBoundary::Port { port }))
            .collect();
        self.assemble_with_boundaries(
            start,
            end,
            design,
            step_s,
            inventory,
            &boundaries,
            port_density_kg_m3,
            grid,
            gas_rt_m2_s2,
            viscosity_pa_s,
            policy,
        )
    }
    pub fn assemble_with_boundaries<S: GeometryTimeBase>(
        &self,
        start: &[S],
        end: &[S],
        design: &[S],
        step_s: S,
        inventory: &[S],
        boundaries: &BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
        port_density_kg_m3: Vec<Vec<S>>,
        grid: TransientVelocityGrid,
        gas_rt_m2_s2: S,
        viscosity_pa_s: S,
        policy: &TransientGasPolicy,
    ) -> CaeResult<NativeGasTransportAssembly<S>> {
        let geometry = self.assemble_geometry(start, end, design, step_s, inventory, boundaries)?;
        let gas = TransientGasInputs {
            volume_m3: geometry.current_geometry.aggregated.volumes_m3.clone(),
            previous_volume_m3: geometry.old_geometry.aggregated.volumes_m3.clone(),
            previous_inventory_kg: geometry.previous_coefficient_inventory,
            faces: geometry.faces,
            port_density_kg_m3,
            grid,
            gas_rt_m2_s2,
            viscosity_pa_s,
            step_s,
        };
        crate::gas_transient_kinetic::validate(&gas, policy)?;
        Ok(NativeGasTransportAssembly {
            inputs: MultiscaleNetworkInputs {
                force_density_n_m3: None,
                gas,
                left_centroid_to_face_m: geometry.left_centroid_to_face_m,
                right_centroid_to_face_m: geometry.right_centroid_to_face_m,
            },
            old_geometry: geometry.old_geometry,
            current_geometry: geometry.current_geometry,
            wall_stages: geometry.wall_stages,
            external_wall_faces: geometry.external_wall_faces,
            maximum_remap_population_defect_kg: geometry.maximum_remap_coefficient_defect,
        })
    }
    pub fn assemble_geometry<S: GeometryTimeBase>(
        &self,
        start: &[S],
        end: &[S],
        design: &[S],
        step_s: S,
        coefficient_inventory: &[S],
        boundaries: &BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
    ) -> CaeResult<NativeGasGeometryAssembly<S>> {
        let path = self.assemble_geometry_path(start, end, design, step_s, boundaries)?;
        let remap = self.layout.remap(
            &path.old_geometry.refined,
            &path.current_geometry.refined,
            coefficient_inventory,
        )?;
        Ok(NativeGasGeometryAssembly {
            step_s,
            previous_coefficient_inventory: remap.inventories_kg,
            faces: path.faces,
            left_centroid_to_face_m: path.left_centroid_to_face_m,
            right_centroid_to_face_m: path.right_centroid_to_face_m,
            old_geometry: path.old_geometry,
            current_geometry: path.current_geometry,
            wall_stages: path.wall_stages,
            external_wall_faces: path.external_wall_faces,
            maximum_remap_coefficient_defect: remap.maximum_population_balance_defect_kg,
        })
    }
    pub fn assemble_geometry_path<S: GeometryTimeBase>(
        &self,
        start: &[S],
        end: &[S],
        design: &[S],
        step_s: S,
        boundaries: &BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
    ) -> CaeResult<NativeGasGeometryPath<S>> {
        self.assemble_geometry_path_selected(start, end, design, step_s, boundaries, None, None)
    }
    pub fn assemble_geometry_path_with_closed_release<S: GeometryTimeBase>(
        &self,
        start: &[S],
        end: &[S],
        design: &[S],
        time_scale: S,
        step_s: S,
        boundaries: &BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
        certificate: &crate::native_closed_release::NativeClosedReleaseCertificate,
    ) -> CaeResult<NativeGasGeometryPath<S>> {
        self.assemble_geometry_path_selected(
            start,
            end,
            design,
            step_s,
            boundaries,
            Some(NativeClosedGeometrySelection::Derivative(certificate)),
            Some(time_scale),
        )
    }
    pub(crate) fn project_and_issue_closed_trial_child<'a,S:GeometryTimeBase>(
        &'a self, macro_start:&[S], macro_end:&[S], design:&[S], time_scale:S,
        sample:std::sync::Arc<crate::native_closed_release::NativeClosedTrialSample>,
    ) -> CaeResult<NativeProjectedTrialChildReceipt<'a,S>> {
        self.kinematics.validate_native_owner_bindings(self.owner.capacity_owner())?;
        if macro_start.len()!=self.owner.capacity_owner().trace_size || macro_end.len()!=macro_start.len()
            || macro_start.iter().chain(macro_end).chain(design).any(|v|!v.finite_first_order())
            || !time_scale.finite_first_order() || time_scale.value()<=0. {
            return Err(fail("invalid parent-projected child inputs"));
        }
        let (child_start,child_end)=self.project_and_sample_closed_trial_geometry_trace(macro_start,macro_end,design,time_scale,&sample)?;
        Ok(NativeProjectedTrialChildReceipt{adapter:self,sample,macro_start:macro_start.to_vec(),macro_end:macro_end.to_vec(),child_start,child_end,design:design.to_vec(),time_scale})
    }
    pub(crate) fn checked_projected_trial_child_pullback_with_original_macro_receipt<'a>(
        &'a self,
        start: &[f64],
        end: &[f64],
        design: &[f64],
        time_scale: f64,
        scope: &NativeGasShapeScope,
        differentiate_time: bool,
        sample: std::sync::Arc<crate::native_closed_release::NativeClosedTrialSample>,
        admit: impl Fn(NativeProjectedTrialChildReceipt<'a, implexity_ad::Dual<16>>) -> CaeResult<()>,
        evaluate: impl FnOnce(
            NativeProjectedTrialChildReceipt<'a,crate::gas_vjp::ReverseScalar>,
        ) -> CaeResult<crate::gas_vjp::ReverseScalar>,
    ) -> CaeResult<NativeGasMacroGeometryPathBars> {
        use crate::gas_vjp::{ReverseScalar, scoped_geometry_pullback};
        use implexity_ad::Dual;
        if scope.maximum_owned_cells == 0
            || self.owner.capacity_owner().grid.cells.len() > scope.maximum_owned_cells
        {
            return Err(fail("invalid selected native geometry reverse scope"));
        }
        for (columns, size) in [
            (&scope.start_columns, start.len()),
            (&scope.end_columns, end.len()),
            (&scope.design_columns, design.len()),
        ] {
            let unique: std::collections::BTreeSet<_> = columns.iter().copied().collect();
            if unique.len() != columns.len() || unique.iter().any(|&i| i >= size) {
                return Err(fail("invalid selected native geometry reverse columns"));
            }
        }
        let mut values = Vec::new();
        for (columns, source) in [
            (&scope.start_columns, start),
            (&scope.end_columns, end),
            (&scope.design_columns, design),
        ] {
            values.extend(columns.iter().map(|&i| source[i]));
        }
        if differentiate_time {
            values.push(time_scale);
        }
        if values.is_empty() || values.len() > 48 {
            return Err(fail(
                "selected native geometry reverse requires 1 to 48 original parameters",
            ));
        }
        self.project_and_issue_closed_trial_child(start,end,design,time_scale,sample.clone())?;
        for base in (0..values.len()).step_by(16) {
            let mut a: Vec<_> = start.iter().copied().map(Dual::<16>::constant).collect();
            let mut b: Vec<_> = end.iter().copied().map(Dual::<16>::constant).collect();
            let mut x: Vec<_> = design.iter().copied().map(Dual::<16>::constant).collect();
            let mut time = Dual::<16>::constant(time_scale);
            let mut offset = 0;
            for (destination, columns) in [
                (&mut a, &scope.start_columns),
                (&mut b, &scope.end_columns),
                (&mut x, &scope.design_columns),
            ] {
                for &column in columns {
                    if offset >= base && offset < base + 16 {
                        destination[column].eps[offset - base] = 1.;
                    }
                    offset += 1;
                }
            }
            if differentiate_time && offset >= base && offset < base + 16 {
                time.eps[offset - base] = 1.;
            }
            let receipt=self.project_and_issue_closed_trial_child(&a,&b,&x,time,sample.clone())?;
            let a=receipt.child_start().to_vec();let b=receipt.child_end().to_vec();
            admit(receipt)?;

        }
        let gradient = scoped_geometry_pullback(&values, |leaves| {
            let mut a: Vec<_> = start.iter().copied().map(ReverseScalar::from_f64).collect();
            let mut b: Vec<_> = end.iter().copied().map(ReverseScalar::from_f64).collect();
            let mut x: Vec<_> = design
                .iter()
                .copied()
                .map(ReverseScalar::from_f64)
                .collect();
            let mut offset = 0;
            for (destination, columns) in [
                (&mut a, &scope.start_columns),
                (&mut b, &scope.end_columns),
                (&mut x, &scope.design_columns),
            ] {
                for &column in columns {
                    destination[column] = leaves[offset];
                    offset += 1;
                }
            }
            let time = if differentiate_time {
                leaves[offset]
            } else {
                ReverseScalar::from_f64(time_scale)
            };
            let receipt=self.project_and_issue_closed_trial_child(&a,&b,&x,time,sample.clone())?;
            let a=receipt.child_start().to_vec();let b=receipt.child_end().to_vec();

            evaluate(receipt)
        })?;
        let mut shape = NativeGasShapeBars {
            start: vec![0.; start.len()],
            end: vec![0.; end.len()],
            design: vec![0.; design.len()],
        };
        let mut offset = 0;
        for (destination, columns) in [
            (&mut shape.start, &scope.start_columns),
            (&mut shape.end, &scope.end_columns),
            (&mut shape.design, &scope.design_columns),
        ] {
            for &column in columns {
                destination[column] = gradient[offset];
                offset += 1;
            }
        }
        Ok(NativeGasMacroGeometryPathBars {
            shape,
            time_scale: if differentiate_time {
                gradient[offset]
            } else {
                0.
            },
        })
    }
    pub(crate) fn project_and_issue_closed_child<'a,S:GeometryTimeBase>(
        &'a self, macro_start:&[S], macro_end:&[S], design:&[S], time_scale:S,
        sample:std::sync::Arc<crate::native_closed_release::NativeClosedReleaseSample>,
    ) -> CaeResult<NativeProjectedChildReceipt<'a,S>> {
        self.kinematics.validate_native_owner_bindings(self.owner.capacity_owner())?;
        if macro_start.len()!=self.owner.capacity_owner().trace_size || macro_end.len()!=macro_start.len()
            || macro_start.iter().chain(macro_end).chain(design).any(|v|!v.finite_first_order())
            || !time_scale.finite_first_order() || time_scale.value()<=0. {
            return Err(fail("invalid parent-projected child inputs"));
        }
        let (child_start,child_end)=self.project_and_sample_closed_geometry_trace(macro_start,macro_end,design,time_scale,&sample)?;
        Ok(NativeProjectedChildReceipt{adapter:self,sample,macro_start:macro_start.to_vec(),macro_end:macro_end.to_vec(),child_start,child_end,design:design.to_vec(),time_scale})
    }
    pub fn project_and_sample_closed_geometry_trace<S: GeometryTimeBase>(
        &self,
        macro_start: &[S],
        macro_end: &[S],
        design: &[S],
        time_scale: S,
        sample: &crate::native_closed_release::NativeClosedReleaseSample,
    ) -> CaeResult<(Vec<S>, Vec<S>)> {
        let (a, b) = self.project_closed_geometry_trace(
            macro_start,
            macro_end,
            design,
            time_scale,
            sample.parent(),
        )?;
        let (start, end) = Self::sample_closed_geometry_trace(&a, &b, sample.fractions());
        sample.validate_path(
            &self.kinematics,
            &macro_start.iter().map(|v| v.value()).collect::<Vec<_>>(),
            &macro_end.iter().map(|v| v.value()).collect::<Vec<_>>(),
            &start.iter().map(|v| v.value()).collect::<Vec<_>>(),
            &end.iter().map(|v| v.value()).collect::<Vec<_>>(),
            &design.iter().map(|v| v.value()).collect::<Vec<_>>(),
        )?;
        Ok((start, end))
    }
    fn sample_closed_geometry_trace<S: GeometryTimeBase>(
        a: &[S],
        b: &[S],
        fractions: (f64, f64),
    ) -> (Vec<S>, Vec<S>) {
        let (alpha, beta) = fractions;
        (
            a.iter()
                .zip(b)
                .map(|(&a, &b)| a + (b - a) * alpha)
                .collect(),
            a.iter().zip(b).map(|(&a, &b)| a + (b - a) * beta).collect(),
        )
    }
    pub fn project_and_sample_closed_trial_geometry_trace<S: GeometryTimeBase>(
        &self,
        macro_start: &[S],
        macro_end: &[S],
        design: &[S],
        time_scale: S,
        sample: &crate::native_closed_release::NativeClosedTrialSample,
    ) -> CaeResult<(Vec<S>, Vec<S>)> {
        let (a, b) = self.project_closed_geometry_trace_owned(
            macro_start,
            macro_end,
            design,
            time_scale,
            sample.parent(),
        )?;
        let (start, end) = Self::sample_closed_geometry_trace(&a, &b, sample.fractions());
        sample.validate_path(
            &self.kinematics,
            &macro_start.iter().map(|v| v.value()).collect::<Vec<_>>(),
            &macro_end.iter().map(|v| v.value()).collect::<Vec<_>>(),
            &start.iter().map(|v| v.value()).collect::<Vec<_>>(),
            &end.iter().map(|v| v.value()).collect::<Vec<_>>(),
            &design.iter().map(|v| v.value()).collect::<Vec<_>>(),
        )?;
        Ok((start, end))
    }
    pub fn assemble_geometry_path_with_closed_trial_sample<S: GeometryTimeBase>(
        &self,
        macro_start: &[S],
        macro_end: &[S],
        design: &[S],
        time_scale: S,
        step_s: S,
        boundaries: &BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
        sample: &crate::native_closed_release::NativeClosedTrialSample,
    ) -> CaeResult<NativeGasGeometryPath<S>> {
        let (start, end) = self.project_and_sample_closed_trial_geometry_trace(
            macro_start,
            macro_end,
            design,
            time_scale,
            sample,
        )?;
        self.assemble_geometry_path_selected(
            &start,
            &end,
            design,
            step_s,
            boundaries,
            Some(NativeClosedGeometrySelection::Trial(sample)),
            None,
        )
    }
    pub fn assemble_geometry_path_with_closed_sample<S: GeometryTimeBase>(
        &self,
        macro_start: &[S],
        macro_end: &[S],
        design: &[S],
        time_scale: S,
        step_s: S,
        boundaries: &BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
        sample: &crate::native_closed_release::NativeClosedReleaseSample,
    ) -> CaeResult<NativeGasGeometryPath<S>> {
        let (start, end) = self.project_and_sample_closed_geometry_trace(
            macro_start,
            macro_end,
            design,
            time_scale,
            sample,
        )?;
        self.assemble_geometry_path_selected(
            &start,
            &end,
            design,
            step_s,
            boundaries,
            Some(NativeClosedGeometrySelection::Sample(sample)),
            None,
        )
    }
    pub fn assemble_geometry_path_with_closed_witness(
        &self,
        start: &[f64],
        end: &[f64],
        design: &[f64],
        step_s: f64,
        boundaries: &BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
        witness: &crate::native_closed_release::NativeClosedGeometryWitness,
    ) -> CaeResult<NativeGasGeometryPath<f64>> {
        self.assemble_geometry_path_selected(
            start,
            end,
            design,
            step_s,
            boundaries,
            Some(NativeClosedGeometrySelection::Primal(witness)),
            None,
        )
    }
    fn assemble_geometry_path_selected<S: GeometryTimeBase>(
        &self,
        start: &[S],
        end: &[S],
        design: &[S],
        step_s: S,
        boundaries: &BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
        closed_release: Option<NativeClosedGeometrySelection<'_>>,
        macro_time_scale: Option<S>,
    ) -> CaeResult<NativeGasGeometryPath<S>> {
        if let Some(selection) = closed_release {
            let a = start.iter().map(|v| v.value()).collect::<Vec<_>>();
            let b = end.iter().map(|v| v.value()).collect::<Vec<_>>();
            let x = design.iter().map(|v| v.value()).collect::<Vec<_>>();
            match selection {
                NativeClosedGeometrySelection::Derivative(certificate) => {
                    certificate.validate_path(&self.kinematics, &a, &b, &x)?;
                    for k in 0..S::direction_count() {
                        certificate.validate_direction(
                            &start.iter().map(|v| v.direction_at(k)).collect::<Vec<_>>(),
                            &end.iter().map(|v| v.direction_at(k)).collect::<Vec<_>>(),
                            Some(&design.iter().map(|v| v.direction_at(k)).collect::<Vec<_>>()),
                            macro_time_scale
                                .ok_or_else(|| {
                                    fail("closed release geometry macro time scale missing")
                                })?
                                .direction_at(k),
                        )?;
                    }
                }
                NativeClosedGeometrySelection::Sample(_)
                | NativeClosedGeometrySelection::Trial(_) => {}
                NativeClosedGeometrySelection::Primal(witness) => {
                    if S::direction_count() != 0
                        || start
                            .iter()
                            .chain(end)
                            .chain(design)
                            .any(|v| v.has_direction())
                    {
                        return Err(fail(
                            "primal closed geometry witness cannot select a derivative",
                        ));
                    }
                    witness.validate_path(&self.kinematics, &a, &b, &x)?;
                }
            }
        }
        if start.len() != end.len() || !step_s.finite_first_order() || step_s.value() <= 0. {
            return Err(fail("invalid native gas trace path"));
        }
        for k in 0..S::direction_count() {
            let x: Vec<_> = design.iter().map(|v| v.value()).collect();
            let dx: Vec<_> = design.iter().map(|v| v.direction_at(k)).collect();
            for trace in [start, end] {
                let u: Vec<_> = trace.iter().map(|v| v.value()).collect();
                let du: Vec<_> = trace.iter().map(|v| v.direction_at(k)).collect();
                self.owner.capacity_owner().capacity_jvp(&u, &x, &du, &dx)?;
            }
        }
        let first = self
            .kinematics
            .mapped_geometry_from_trace(start, design, |v, _| Ok(v))?;
        let last = self
            .kinematics
            .mapped_geometry_from_trace(end, design, |v, _| Ok(v))?;
        if first.len() != self.paired_vertex_ids.len() || last.len() != first.len() {
            return Err(fail("native gas path metadata shape"));
        }
        let triangles: Vec<_> = first
            .iter()
            .zip(&last)
            .zip(&self.paired_vertex_ids)
            .map(
                |((&start_m, &end_m), &paired_vertex_ids)| LayeredGapTriangle {
                    start_m,
                    end_m,
                    paired_vertex_ids,
                },
            )
            .collect();
        let path_mesh = if let Some(certificate) = closed_release {
            match certificate {
                NativeClosedGeometrySelection::Derivative(certificate) => {
                    gas_layered_tetra_mesh::build_closed_release(
                        &triangles,
                        self.layers,
                        step_s,
                        self.length_tolerance_m,
                        certificate,
                        false,
                    )?
                }
                NativeClosedGeometrySelection::Primal(witness) => {
                    gas_layered_tetra_mesh::build_closed_witness(
                        &triangles,
                        self.layers,
                        step_s,
                        self.length_tolerance_m,
                        witness,
                        false,
                    )?
                }
                NativeClosedGeometrySelection::Sample(sample) => {
                    gas_layered_tetra_mesh::build_closed_sample(
                        &triangles,
                        self.layers,
                        step_s,
                        self.length_tolerance_m,
                        sample,
                    )?
                }
                NativeClosedGeometrySelection::Trial(sample) => {
                    gas_layered_tetra_mesh::build_closed_trial_sample(
                        &triangles,
                        self.layers,
                        step_s,
                        self.length_tolerance_m,
                        sample,
                    )?
                }
            }
        } else {
            gas_layered_tetra_mesh::build(&triangles, self.layers, step_s, self.length_tolerance_m)?
        };
        let old_geometry = if let Some(certificate) = closed_release {
            let tets = self.chart_subdomains_selected(start, design, Some(certificate), 0.)?;
            self.endpoint_with_regions(start, design, &tets)?
        } else {
            self.endpoint(start, design)?
        };
        let current_geometry = if let Some(certificate) = closed_release {
            let tets = self.chart_subdomains_selected(end, design, Some(certificate), 1.)?;
            self.endpoint_with_regions(end, design, &tets)?
        } else {
            self.endpoint(end, design)?
        };
        let owner = self.owner.capacity_owner();
        let coordinate_scale = owner
            .grid
            .origin_m
            .iter()
            .map(|v| v.abs())
            .fold(owner.grid.spacing_m, f64::max);
        let roundoff = 8192.
            * f64::EPSILON
            * (coordinate_scale / owner.grid.spacing_m).max(1.)
            * (1. + owner.grid.cells.len() as f64);
        let direction_scales: Vec<_> = (0..S::direction_count())
            .map(|k| {
                let motion = start
                    .iter()
                    .chain(end)
                    .map(|v| v.direction_at(k).abs())
                    .fold(0., f64::max);
                let phase = design
                    .iter()
                    .map(|v| v.direction_at(k).abs())
                    .fold(0., f64::max);
                roundoff
                    * (owner.grid.spacing_m.powi(2) * motion + owner.grid.spacing_m.powi(3) * phase)
            })
            .collect();
        for (endpoint_id, endpoint) in [&old_geometry, &current_geometry].into_iter().enumerate() {
            for (defect_id, defect) in endpoint.partition_defects_m3.iter().enumerate() {
                for (k, &bound) in direction_scales.iter().enumerate() {
                    let d = defect.direction_at(k);
                    if !d.is_finite() || d.abs() > bound {
                        return Err(fail(&format!(
                            "native gas geometry direction crosses a clipping stratum endpoint={endpoint_id} index={defect_id} cells={} d={d:e} bound={bound:e} defect={defect:?}",
                            owner.grid.cells.len()
                        )));
                    }
                }
            }
        }
        let source_closed_stage=closed_release.map(|selection|Ok::<_,implexity_core::CaeError>(NativeSourceClosedStage{
            adapter_identity:self as *const NativeGasGeometryAdapter as usize,selection,
            start_bits:source_scalar_bits(start)?,end_bits:source_scalar_bits(end)?,design_bits:source_scalar_bits(design)?,
            step_bits:source_scalar_bits(&[step_s])?,
            macro_time_bits:macro_time_scale.map(|t|source_scalar_bits(&[t])).transpose()?,
        })).transpose()?;
        let mut faces = vec![];
        let mut wall_interior_anchors = vec![];
        let mut wall_anchor_routes = vec![];
        let mut left_offset = vec![];
        let mut right_offset = vec![];
        let mut wall_stages = vec![];
        let mut external_wall_faces = vec![];
        let mut wall_id = 0usize;
        for alpha in [0.5 - 0.5 / 3f64.sqrt(), 0.5 + 0.5 / 3f64.sqrt()] {
            let trace: Vec<_> = start
                .iter()
                .zip(end)
                .map(|(&a, &b)| GeometryTime::new(a + (b - a) * alpha, (b - a) / step_s))
                .collect();
            let x: Vec<_> = design
                .iter()
                .map(|&v| GeometryTime::new(v, S::zero()))
                .collect();
            let source=crate::native_source_face_windows::NativeSourceFaceWindows::from_stage(
                self,start,end,design,step_s,macro_time_scale.map(|t|t.value()).unwrap_or(1.),alpha,
                source_closed_stage.as_ref().map(crate::native_closed_stage::ClosedStage::Adapter),
            )?;
            let (stage,aperture_dispatch)=self.endpoint_with_source_dispatch(source.live_stage_trace(),source.live_stage_design(),source.stage_tets(),true)?;
            for defect in &stage.partition_defects_m3 {
                for (k, &bound) in direction_scales.iter().enumerate() {
                    let d = defect.value.direction_at(k);
                    if !d.is_finite() || d.abs() > bound {
                        return Err(fail(
                            "native gas stage direction crosses a clipping stratum",
                        ));
                    }
                }
            }
            let endpoint_trace: Vec<_> = trace.iter().map(|v| v.value).collect();
            for q in stage.refined.partition.aperture_quadrature() {
                let a = &stage.refined.partition.apertures[q.aperture];
                let left=stage.aggregated.component_slots[q.left];let right=q.right.map(|i|stage.aggregated.component_slots[i]);
                if right==Some(left){continue;}
                if a.axis.is_none(){
                    let dispatch=aperture_dispatch.as_ref().ok_or_else(||fail("source stage aperture dispatch is absent"))?;
                    let incidence=dispatch.subdomains(&stage.refined,q.aperture)?;
                    let mut source_windows=std::collections::BTreeSet::new();
                    if let Some(incidence)=incidence {
                        for &(region,face) in incidence {
                            source_windows.insert(source.layered_window_for_subdomain(region,face).ok_or_else(||fail("actual aperture source face is absent"))?);
                        }
                    }
                    if source_windows.len()>1{return Err(fail("actual aperture source owners belong to distinct layered windows"));}
                    if !source_windows.is_empty(){continue;}
                    if stage.refined.regions[q.left].is_some()||q.right.is_some_and(|i|stage.refined.regions[i].is_some()){
                        return Err(fail("layered aperture lacks authenticated actual source incidence"));
                    }
                }
                let boundary = if let Some(right) = right {
                    TransientGasBoundary::Interior { right }
                } else {
                    let axis = a
                        .axis
                        .ok_or_else(|| fail("unowned external numerical gas interface"))?;
                    let side = usize::from(a.normal[axis].value() >= 0.);
                    match *boundaries
                        .get(&(a.outside_cell, axis, side))
                        .ok_or_else(|| fail("native gas exterior has no declared boundary"))?
                    {
                        NativeGasExteriorBoundary::Port { port } => {
                            TransientGasBoundary::Port { port }
                        }
                        NativeGasExteriorBoundary::StationaryWall => {
                            external_wall_faces.push(faces.len());
                            let wall = wall_id;
                            wall_id += 1;
                            TransientGasBoundary::DiffuseWall { wall }
                        }
                    }
                };
                let position = q.position_m.map(|v| v.value);
                left_offset.push(std::array::from_fn(|k| {
                    position[k] - current_geometry.aggregated.centroids_m[left][k]
                }));
                right_offset.push(right.map(|j| {
                    std::array::from_fn(|k| {
                        position[k] - current_geometry.aggregated.centroids_m[j][k]
                    })
                }));
                let ale_velocity = if a.axis.is_some() {
                    [S::zero(); 3]
                } else {
                    let region = stage.refined.regions[q.left]
                        .or_else(|| q.right.and_then(|r| stage.refined.regions[r]))
                        .ok_or_else(|| fail("numerical gas aperture has no velocity chart"))?;
                    let tet = path_mesh
                        .tetrahedra
                        .iter()
                        .find(|t| (t.chart * self.layers + t.layer) * 3 + t.local_tetra == region)
                        .ok_or_else(|| fail("numerical gas velocity chart is missing"))?;
                    let vertices = tet.nodes.map(|i| {
                        std::array::from_fn(|k| {
                            path_mesh.nodes[i].start_m[k] * (1. - alpha)
                                + path_mesh.nodes[i].end_m[k] * alpha
                        })
                    });
                    let weights = material_barycentric(position, vertices)?;
                    std::array::from_fn(|k| {
                        (0..4)
                            .map(|j| {
                                (path_mesh.nodes[tet.nodes[j]].end_m[k]
                                    - path_mesh.nodes[tet.nodes[j]].start_m[k])
                                    * weights[j]
                                    / step_s
                            })
                            .fold(S::zero(), |a, b| a + b)
                    })
                };
                wall_interior_anchors.push(None);
                wall_anchor_routes.push(serde_json::Value::Null);
                faces.push(TransientGasFace {
                    left,
                    boundary: boundary.clone(),
                    area_m2: q.area_m2.value * 0.5,
                    normal: q.normal.map(|v| v.value),
                    ale_velocity_m_s: if matches!(
                        boundary,
                        TransientGasBoundary::DiffuseWall { .. }
                    ) {
                        [S::zero(); 3]
                    } else {
                        ale_velocity
                    },
                    wall_velocity_m_s: [S::zero(); 3],
                });
            }
            let source_labels=crate::native_source_stage_labels::NativeSourceStageLabels::new(&source)?;
            let prepared=source_labels.prepare()?;
            let source_walls=crate::native_source_wall_stage::NativeSourceWallPreparation::new(&prepared)?;
            for window in 0..source.windows().len(){
                if let crate::native_source_face_windows::SourceWindowKind::Layered{incidences}=source.windows()[window].kind(){
                    if source.layered_window_is_fixed_grid_owned(window)?{continue;}
                    let samples=prepared.prepare_window(window,0,[0.93,0.37])?;
                    for sample in samples{
                        let (left,right)=match sample.label{
                            crate::native_source_stage_labels::SourceFaceLabel::LayeredInterface{minus,plus}=>(minus,plus),
                            _=>return Err(fail("source layered runtime packet is not a two-fluid interface")),
                        };
                        if left==right{return Err(fail("source layered runtime packet repeats one slot"));}
                        let position=sample.position_m.map(|v|v.value);
                        let region=incidences.iter().find(|(_,side)|*side==1).map(|(r,_)|*r)
                            .or_else(||incidences.first().map(|(r,_)|*r)).ok_or_else(||fail("source layered runtime incidence is absent"))?;
                        let tet=path_mesh.tetrahedra.iter().find(|t|(t.chart*self.layers+t.layer)*3+t.local_tetra==region)
                            .ok_or_else(||fail("source layered runtime velocity chart is absent"))?;
                        let vertices=tet.nodes.map(|i|std::array::from_fn(|k|path_mesh.nodes[i].start_m[k]*(1.-alpha)+path_mesh.nodes[i].end_m[k]*alpha));
                        let weights=material_barycentric(position,vertices)?;
                        let velocity=std::array::from_fn(|k|(0..4).map(|j|(path_mesh.nodes[tet.nodes[j]].end_m[k]-path_mesh.nodes[tet.nodes[j]].start_m[k])*weights[j]/step_s).fold(S::zero(),|a,b|a+b));
                        left_offset.push(std::array::from_fn(|k|position[k]-current_geometry.aggregated.centroids_m[left][k]));
                        right_offset.push(Some(std::array::from_fn(|k|position[k]-current_geometry.aggregated.centroids_m[right][k])));
                        wall_interior_anchors.push(None);wall_anchor_routes.push(serde_json::Value::Null);
                        faces.push(TransientGasFace{left,boundary:TransientGasBoundary::Interior{right},area_m2:sample.area_m2.value*0.5,
                            normal:sample.normal.map(|v|v.value),ale_velocity_m_s:velocity,wall_velocity_m_s:[S::zero();3]});
                    }
                    continue;
                }
                let samples=prepared.prepare_window(window,0,[0.93,0.37])?;
                let owned=source_walls.prepare_samples(window,&samples)?;
                if samples.len()!=owned.quadrature.len(){return Err(fail("source runtime wall packet dimensions differ"));}
                if samples.is_empty(){continue;}
                let motion=owned.transfer.kinematics(start,end,alpha,step_s)?;
                let mut indices=Vec::with_capacity(samples.len());
                for(i,sample)in samples.iter().enumerate(){
                    let q=&owned.quadrature[i];let left=owned.fluid_slots[i];
                    let(component,part,plane)=owned.convex_supports[i];
                    let support=stage.wall_convex_support.iter().find(|s|s.component==component&&s.convex_part==part)
                        .ok_or_else(||fail("source runtime selected wall support is absent"))?;
                    let centroid=*current_geometry.aggregated.centroids_m.get(left).ok_or_else(||fail("source runtime current centroid is absent"))?;
                    let offset=std::array::from_fn(|k|q.position_m[k]-centroid[k]);
                    let anchor=self.source_wall_anchor(support,&prepared,window,sample,centroid)?;
                    let nominated=anchor.is_some();
                    let bound_position:[S;3]=std::array::from_fn(|k|centroid[k]+offset[k]);
                    wall_anchor_routes.push(serde_json::json!([
                        alpha.to_bits(),window,component,part,q.body,q.native_nodes,i,nominated,
                        bound_position.map(|v|v.value().to_bits()),q.normal.map(|v|v.value().to_bits()),
                        anchor.as_ref().map(|a|a.position_m().map(|v|v.value().to_bits())),plane
                    ]));
                    left_offset.push(offset);right_offset.push(None);wall_interior_anchors.push(anchor);
                    indices.push(faces.len());
                    faces.push(TransientGasFace{left,boundary:TransientGasBoundary::DiffuseWall{wall:wall_id},
                        area_m2:q.area_m2*0.5,normal:q.normal,ale_velocity_m_s:motion.velocity_m_s[i],wall_velocity_m_s:motion.velocity_m_s[i]});
                    wall_id+=1;
                }
                wall_stages.push(NativeGasWallStage{transfer:owned.transfer,face_indices:indices});
            }
        }
        let wall_anchor_route_identity = implexity_core::json::canonical_sha256(
            &serde_json::json!(wall_anchor_routes),
        );
        Ok(NativeGasGeometryPath {
            birth_authority:None,
            birth_input_leaves:start.iter().chain(end).chain(design).copied().chain(std::iter::once(step_s)).collect(),
            birth_regions:Vec::new(),birth_route_identity:String::new(),
            wall_interior_anchors,
            wall_anchor_route_identity,
            step_s,
            faces,
            left_centroid_to_face_m: left_offset,
            right_centroid_to_face_m: right_offset,
            old_geometry,
            current_geometry,
            wall_stages,
            external_wall_faces,
        })
    }
}
pub struct NativeReferencedGasWallLoad<S: TransientGasScalar> {
    pub load: NativeGasWallLoad<S>,
    pub reference_impulse_n_s: [S; 3],
    pub reference_work_j: S,
}
struct NativeGasGeometryView<'a, S: GeometryTimeBase> {
    step_s: S,
    faces: &'a [TransientGasFace<S>],
    left_centroid_to_face_m: &'a [[S; 3]],
    right_centroid_to_face_m: &'a [Option<[S; 3]>],
    old_geometry: &'a NativeGasGeometryEndpoint<S>,
    current_geometry: &'a NativeGasGeometryEndpoint<S>,
    wall_stages: &'a [NativeGasWallStage<S>],
}
impl<S: GeometryTimeBase> NativeGasGeometryView<'_, S> {
    pub fn certify_stationary_continuum_slots(&self, slots: &[usize]) -> CaeResult<()> {
        use crate::native_cut_cell_partition::CutBoundary;
        let mut selected = std::collections::BTreeSet::new();
        for &slot in slots {
            if !selected.insert(slot) || slot >= self.current_geometry.aggregated.volumes_m3.len() {
                return Err(fail("invalid continuum gas slot selection"));
            }
            let mut identity = None;
            for geometry in [self.old_geometry, self.current_geometry] {
                let indices: Vec<_> = geometry
                    .aggregated
                    .component_slots
                    .iter()
                    .enumerate()
                    .filter_map(|(i, &s)| (s == slot).then_some(i))
                    .collect();
                if indices.len() != 1 {
                    return Err(fail(
                        "continuum gas slot is not one persistent full-fluid cell",
                    ));
                }
                let index = indices[0];
                let component = &geometry.refined.partition.components[index];
                if geometry.refined.regions[index].is_some()
                    || component.convex_parts.len() != 1
                    || component.convex_parts[0]
                        .iter()
                        .any(|p| !matches!(p.boundary, CutBoundary::Cell { .. }))
                {
                    return Err(fail(
                        "continuum gas slot contains moving or refined geometry",
                    ));
                }
                let key = (component.cell, component.component);
                if identity.is_some_and(|old| old != key) {
                    return Err(fail("continuum gas component identity changed"));
                }
                identity = Some(key);
                if !geometry.aggregated.active_slots[slot]
                    || geometry.aggregated.volumes_m3[slot].has_direction()
                    || geometry.aggregated.centroids_m[slot]
                        .iter()
                        .any(|v| v.has_direction())
                {
                    return Err(fail("continuum gas geometry is not stationary"));
                }
            }
            if self.old_geometry.aggregated.volumes_m3[slot].value()
                != self.current_geometry.aggregated.volumes_m3[slot].value()
                || self.old_geometry.aggregated.centroids_m[slot].map(|v| v.value())
                    != self.current_geometry.aggregated.centroids_m[slot].map(|v| v.value())
            {
                return Err(fail("continuum gas capacity changed"));
            }
        }
        for (i, face) in self.faces.iter().enumerate() {
            let right = match face.boundary {
                TransientGasBoundary::Interior { right } => Some(right),
                _ => None,
            };
            if selected.contains(&face.left) || right.is_some_and(|r| selected.contains(&r)) {
                if face.area_m2.has_direction()
                    || face.normal.iter().any(|v| v.has_direction())
                    || face
                        .ale_velocity_m_s
                        .iter()
                        .chain(&face.wall_velocity_m_s)
                        .any(|v| v.value() != 0. || v.has_direction())
                    || self.wall_stages.iter().any(|s| s.face_indices.contains(&i))
                {
                    return Err(fail("continuum gas interface is not stationary"));
                }
                if selected.contains(&face.left)
                    && self.left_centroid_to_face_m[i]
                        .iter()
                        .any(|v| v.has_direction())
                {
                    return Err(fail("continuum gas face offset changes"));
                }
                if right.is_some_and(|r| selected.contains(&r))
                    && self.right_centroid_to_face_m[i]
                        .is_none_or(|o| o.iter().any(|v| v.has_direction()))
                {
                    return Err(fail("continuum gas neighbor offset changes"));
                }
            }
        }
        Ok(())
    }

    pub fn scatter_wall_packets(
        &self,
        packets: &[(usize, TransientGasPacket<S>)],
        start: &[S],
        end: &[S],
    ) -> CaeResult<NativeGasWallLoad<S>> {
        Ok(self
            .scatter_wall_packets_with_reference(packets, start, end, S::zero())?
            .load)
    }
    pub fn scatter_wall_packets_with_reference(
        &self,
        packets: &[(usize, TransientGasPacket<S>)],
        start: &[S],
        end: &[S],
        reference_pressure_pa: S,
    ) -> CaeResult<NativeReferencedGasWallLoad<S>> {
        let mut by_face = BTreeMap::new();
        for (i, p) in packets {
            if *i >= self.faces.len() || by_face.insert(*i, p).is_some() {
                return Err(fail("invalid duplicate native gas face packet"));
            }
        }
        let mut force = vec![S::zero(); start.len()];
        let mut impulse = [S::zero(); 3];
        let mut work = S::zero();
        let mut reference_impulse = [S::zero(); 3];
        let mut reference_work = S::zero();
        for stage in self.wall_stages {
            let impulses = stage
                .face_indices
                .iter()
                .map(|i| {
                    let packet = by_face
                        .get(i)
                        .ok_or_else(|| fail("native gas wall packet is absent"))?;
                    let face = &self.faces[*i];
                    let adjustment = crate::reference_traction::adjust_wall_reference(
                        packet.momentum_kg_m_s,
                        face.normal.map(|n| n * face.area_m2 * self.step_s),
                        face.wall_velocity_m_s,
                        reference_pressure_pa,
                    )?;
                    for k in 0..3 {
                        reference_impulse[k] += adjustment.reference_impulse_kg_m_s[k];
                    }
                    reference_work += adjustment.reference_work_j;
                    Ok(adjustment.momentum_impulse_kg_m_s)
                })
                .collect::<CaeResult<Vec<_>>>()?;
            let load = stage.transfer.scatter(&impulses, start, end, self.step_s)?;
            for (a, b) in force.iter_mut().zip(load.force_flux_n) {
                *a += b;
            }
            for k in 0..3 {
                impulse[k] += load.impulse_n_s[k];
            }
            work += load.wall_work_j;
        }
        Ok(NativeReferencedGasWallLoad {
            load: NativeGasWallLoad {
                force_flux_n: force,
                impulse_n_s: impulse,
                wall_work_j: work,
            },
            reference_impulse_n_s: reference_impulse,
            reference_work_j: reference_work,
        })
    }
}
impl<S: GeometryTimeBase> NativeGasTransportAssembly<S> {
    fn geometry_view(&self) -> NativeGasGeometryView<'_, S> {
        NativeGasGeometryView {
            step_s: self.inputs.gas.step_s,
            faces: &self.inputs.gas.faces,
            left_centroid_to_face_m: &self.inputs.left_centroid_to_face_m,
            right_centroid_to_face_m: &self.inputs.right_centroid_to_face_m,
            old_geometry: &self.old_geometry,
            current_geometry: &self.current_geometry,
            wall_stages: &self.wall_stages,
        }
    }
    pub fn certify_stationary_continuum_slots(&self, slots: &[usize]) -> CaeResult<()> {
        self.geometry_view()
            .certify_stationary_continuum_slots(slots)
    }
    pub fn scatter_wall_packets(
        &self,
        packets: &[(usize, TransientGasPacket<S>)],
        start: &[S],
        end: &[S],
    ) -> CaeResult<NativeGasWallLoad<S>> {
        self.geometry_view()
            .scatter_wall_packets(packets, start, end)
    }
    pub fn scatter_wall_packets_with_reference(
        &self,
        packets: &[(usize, TransientGasPacket<S>)],
        start: &[S],
        end: &[S],
        reference_pressure_pa: S,
    ) -> CaeResult<NativeReferencedGasWallLoad<S>> {
        self.geometry_view().scatter_wall_packets_with_reference(
            packets,
            start,
            end,
            reference_pressure_pa,
        )
    }
}
impl<S: GeometryTimeBase> NativeGasGeometryAssembly<S> {
    fn geometry_view(&self) -> NativeGasGeometryView<'_, S> {
        NativeGasGeometryView {
            step_s: self.step_s,
            faces: &self.faces,
            left_centroid_to_face_m: &self.left_centroid_to_face_m,
            right_centroid_to_face_m: &self.right_centroid_to_face_m,
            old_geometry: &self.old_geometry,
            current_geometry: &self.current_geometry,
            wall_stages: &self.wall_stages,
        }
    }
    pub fn certify_stationary_continuum_slots(&self, slots: &[usize]) -> CaeResult<()> {
        self.geometry_view()
            .certify_stationary_continuum_slots(slots)
    }
    pub fn scatter_wall_packets(
        &self,
        packets: &[(usize, TransientGasPacket<S>)],
        start: &[S],
        end: &[S],
    ) -> CaeResult<NativeGasWallLoad<S>> {
        self.geometry_view()
            .scatter_wall_packets(packets, start, end)
    }
    pub fn scatter_wall_packets_with_reference(
        &self,
        packets: &[(usize, TransientGasPacket<S>)],
        start: &[S],
        end: &[S],
        reference_pressure_pa: S,
    ) -> CaeResult<NativeReferencedGasWallLoad<S>> {
        self.geometry_view().scatter_wall_packets_with_reference(
            packets,
            start,
            end,
            reference_pressure_pa,
        )
    }
}
impl<S: GeometryTimeBase> NativeGasGeometryPath<S> {
    pub fn wall_anchor_route_identity(&self) -> &str { &self.wall_anchor_route_identity }
    pub(crate) fn validate_wall_anchor_route<T: GeometryTimeBase>(&self, reference: &NativeGasGeometryPath<T>) -> CaeResult<()> {
        if self.birth_route_identity!=reference.birth_route_identity{return Err(fail("native birth route differs from accepted primal"));}
        if self.wall_anchor_route_identity != reference.wall_anchor_route_identity
            || self.wall_interior_anchors.len() != self.faces.len()
            || reference.wall_interior_anchors.len() != reference.faces.len()
            || self.wall_interior_anchors.len() != reference.wall_interior_anchors.len()
            || self.wall_interior_anchors.iter().zip(&reference.wall_interior_anchors)
                .any(|(a,b)|a.is_some()!=b.is_some())
        { return Err(fail("native wall interior anchor route ownership differs")); }
        Ok(())
    }
    fn geometry_view(&self) -> NativeGasGeometryView<'_, S> {
        NativeGasGeometryView {
            step_s: self.step_s,
            faces: &self.faces,
            left_centroid_to_face_m: &self.left_centroid_to_face_m,
            right_centroid_to_face_m: &self.right_centroid_to_face_m,
            old_geometry: &self.old_geometry,
            current_geometry: &self.current_geometry,
            wall_stages: &self.wall_stages,
        }
    }
    pub fn certify_stationary_continuum_slots(&self, slots: &[usize]) -> CaeResult<()> {
        self.geometry_view()
            .certify_stationary_continuum_slots(slots)
    }
    pub fn scatter_wall_packets(
        &self,
        packets: &[(usize, TransientGasPacket<S>)],
        start: &[S],
        end: &[S],
    ) -> CaeResult<NativeGasWallLoad<S>> {
        self.geometry_view()
            .scatter_wall_packets(packets, start, end)
    }
    pub fn scatter_wall_packets_with_reference(
        &self,
        packets: &[(usize, TransientGasPacket<S>)],
        start: &[S],
        end: &[S],
        reference_pressure_pa: S,
    ) -> CaeResult<NativeReferencedGasWallLoad<S>> {
        self.geometry_view().scatter_wall_packets_with_reference(
            packets,
            start,
            end,
            reference_pressure_pa,
        )
    }
}
#[derive(Clone, Debug)]
pub struct NativeGasShapeScope {
    pub start_columns: Vec<usize>,
    pub end_columns: Vec<usize>,
    pub design_columns: Vec<usize>,
    pub maximum_owned_cells: usize,
}
#[derive(Clone, Debug)]
pub struct NativeGasShapeBars {
    pub start: Vec<f64>,
    pub end: Vec<f64>,
    pub design: Vec<f64>,
}
impl NativeGasGeometryAdapter {
    pub(crate) fn checked_shape_pullback(
        &self,
        start: &[f64],
        end: &[f64],
        design: &[f64],
        step_s: f64,
        inventory: &[f64],
        boundaries: &BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
        port_density_kg_m3: Vec<Vec<f64>>,
        grid: TransientVelocityGrid,
        gas_rt_m2_s2: f64,
        viscosity_pa_s: f64,
        policy: &TransientGasPolicy,
        scope: &NativeGasShapeScope,
        evaluate: impl FnOnce(
            &NativeGasTransportAssembly<crate::gas_vjp::ReverseScalar>,
            &[crate::gas_vjp::ReverseScalar],
            &[crate::gas_vjp::ReverseScalar],
        ) -> CaeResult<crate::gas_vjp::ReverseScalar>,
    ) -> CaeResult<NativeGasShapeBars> {
        use crate::gas_vjp::{ReverseScalar, scoped_pullback};
        let owner = self.owner.capacity_owner();
        if scope.maximum_owned_cells == 0 || owner.grid.cells.len() > scope.maximum_owned_cells {
            return Err(fail("native gas shape scope exceeds its owned-cell budget"));
        }
        for (columns, size) in [
            (&scope.start_columns, start.len()),
            (&scope.end_columns, end.len()),
            (&scope.design_columns, design.len()),
        ] {
            let unique: std::collections::BTreeSet<_> = columns.iter().copied().collect();
            if unique.len() != columns.len() || unique.iter().any(|&i| i >= size) {
                return Err(fail("invalid native gas shape columns"));
            }
        }
        let activity = vec![1.; owner.grid.cells.len()];
        for (trace, columns) in [(start, &scope.start_columns), (end, &scope.end_columns)] {
            let nonsmooth = owner.coincident_face_trace_columns(trace, design, &activity)?;
            if columns.iter().any(|i| nonsmooth.contains(i)) {
                return Err(fail("native gas shape scope crosses a native box-face tie"));
            }
            let tied = owner.iso_face_design_columns(trace, design, &activity)?;
            if scope.design_columns.iter().any(|i| tied.contains(i)) {
                return Err(fail(
                    "native gas shape scope crosses a native phase-face tie",
                ));
            }
        }
        let primal = self.assemble_with_boundaries(
            start,
            end,
            design,
            step_s,
            inventory,
            boundaries,
            port_density_kg_m3.clone(),
            grid.clone(),
            gas_rt_m2_s2,
            viscosity_pa_s,
            policy,
        )?;
        if primal.old_geometry.aggregated.active_slots
            != primal.current_geometry.aggregated.active_slots
            || primal.old_geometry.aggregated.component_slots
                != primal.current_geometry.aggregated.component_slots
            || primal.old_geometry.refined.regions != primal.current_geometry.refined.regions
        {
            return Err(fail(
                "native gas shape reverse requires a fixed component stratum",
            ));
        }
        drop(primal);
        let values: Vec<_> = scope
            .start_columns
            .iter()
            .map(|&i| start[i])
            .chain(scope.end_columns.iter().map(|&i| end[i]))
            .chain(scope.design_columns.iter().map(|&i| design[i]))
            .collect();
        let coordinate_scale = owner
            .grid
            .origin_m
            .iter()
            .map(|v| v.abs())
            .fold(owner.grid.spacing_m, f64::max);
        let roundoff = 8192.
            * f64::EPSILON
            * (coordinate_scale / owner.grid.spacing_m).max(1.)
            * (1. + owner.grid.cells.len() as f64);
        for base in (0..values.len()).step_by(16) {
            use implexity_ad::Dual;
            let mut a: Vec<_> = start.iter().copied().map(Dual::<16>::constant).collect();
            let mut b: Vec<_> = end.iter().copied().map(Dual::<16>::constant).collect();
            let mut x: Vec<_> = design.iter().copied().map(Dual::<16>::constant).collect();
            let mut offset = 0;
            for (destination, columns) in [
                (&mut a, &scope.start_columns),
                (&mut b, &scope.end_columns),
                (&mut x, &scope.design_columns),
            ] {
                for &column in columns {
                    if offset >= base && offset < base + 16 {
                        destination[column].eps[offset - base] = 1.;
                    }
                    offset += 1;
                }
            }
            for alpha in [0., 0.5 - 0.5 / 3f64.sqrt(), 0.5 + 0.5 / 3f64.sqrt(), 1.] {
                let trace: Vec<_> = a
                    .iter()
                    .zip(&b)
                    .map(|(&a, &b)| a * (1. - alpha) + b * alpha)
                    .collect();
                let endpoint = self.endpoint(&trace, &x)?;
                for defect in endpoint.partition_defects_m3 {
                    for (j, &value) in defect.eps.iter().enumerate() {
                        let scale =
                            if base + j < scope.start_columns.len() + scope.end_columns.len() {
                                owner.grid.spacing_m.powi(2)
                            } else {
                                owner.grid.spacing_m.powi(3)
                            };
                        if !value.is_finite() || value.abs() > roundoff * scale {
                            return Err(fail(
                                "native gas shape direction crosses a clipping or capacity stratum",
                            ));
                        }
                    }
                }
            }
        }
        let gradient = scoped_pullback(&values, |leaves| {
            let mut a: Vec<_> = start.iter().copied().map(ReverseScalar::from_f64).collect();
            let mut b: Vec<_> = end.iter().copied().map(ReverseScalar::from_f64).collect();
            let mut x: Vec<_> = design
                .iter()
                .copied()
                .map(ReverseScalar::from_f64)
                .collect();
            let mut offset = 0;
            for (destination, columns) in [
                (&mut a, &scope.start_columns),
                (&mut b, &scope.end_columns),
                (&mut x, &scope.design_columns),
            ] {
                for &column in columns {
                    destination[column] = leaves[offset];
                    offset += 1;
                }
            }
            let q: Vec<_> = inventory
                .iter()
                .copied()
                .map(ReverseScalar::from_f64)
                .collect();
            let ports = port_density_kg_m3
                .iter()
                .map(|p| p.iter().copied().map(ReverseScalar::from_f64).collect())
                .collect();
            let assembly = self.assemble_with_boundaries(
                &a,
                &b,
                &x,
                ReverseScalar::from_f64(step_s),
                &q,
                boundaries,
                ports,
                grid,
                ReverseScalar::from_f64(gas_rt_m2_s2),
                ReverseScalar::from_f64(viscosity_pa_s),
                policy,
            )?;
            evaluate(&assembly, &a, &b)
        })?;
        let mut bars = NativeGasShapeBars {
            start: vec![0.; start.len()],
            end: vec![0.; end.len()],
            design: vec![0.; design.len()],
        };
        let mut offset = 0;
        for (destination, columns) in [
            (&mut bars.start, &scope.start_columns),
            (&mut bars.end, &scope.end_columns),
            (&mut bars.design, &scope.design_columns),
        ] {
            for &column in columns {
                destination[column] = gradient[offset];
                offset += 1;
            }
        }
        Ok(bars)
    }
}

#[derive(Clone, Debug)]
pub struct NativeGasGeometryPathBars {
    pub shape: NativeGasShapeBars,
    pub step_s: f64,
}
impl NativeGasGeometryAdapter {
    pub(crate) fn checked_path_pullback(
        &self,
        start: &[f64],
        end: &[f64],
        design: &[f64],
        step_s: f64,
        boundaries: &BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
        scope: &NativeGasShapeScope,
        differentiate_step: bool,
        evaluate: impl FnOnce(
            &NativeGasGeometryPath<crate::gas_vjp::ReverseScalar>,
            &[crate::gas_vjp::ReverseScalar],
            &[crate::gas_vjp::ReverseScalar],
            &[crate::gas_vjp::ReverseScalar],
        ) -> CaeResult<crate::gas_vjp::ReverseScalar>,
    ) -> CaeResult<NativeGasGeometryPathBars> {
        use crate::gas_vjp::{ReverseScalar, scoped_geometry_pullback};
        use implexity_ad::Dual;
        let owner = self.owner.capacity_owner();
        if scope.maximum_owned_cells == 0 || owner.grid.cells.len() > scope.maximum_owned_cells {
            return Err(fail("native path derivative exceeds its owned-cell scope"));
        }
        let mut count = usize::from(differentiate_step);
        for (columns, size) in [
            (&scope.start_columns, start.len()),
            (&scope.end_columns, end.len()),
            (&scope.design_columns, design.len()),
        ] {
            let unique = columns
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
            if unique.len() != columns.len() || unique.iter().any(|&i| i >= size) {
                return Err(fail("native path derivative columns invalid"));
            }
            count = count
                .checked_add(columns.len())
                .ok_or_else(|| fail("native path derivative dimension overflow"))?;
        }
        if count > 48 {
            return Err(fail(
                "native path derivative exceeds its local parameter budget",
            ));
        }
        let activity = vec![1.; owner.grid.cells.len()];
        for (trace, columns) in [(start, &scope.start_columns), (end, &scope.end_columns)] {
            let box_ties = owner.coincident_face_trace_columns(trace, design, &activity)?;
            let phase_ties = owner.iso_face_design_columns(trace, design, &activity)?;
            if columns.iter().any(|i| box_ties.contains(i))
                || scope.design_columns.iter().any(|i| phase_ties.contains(i))
            {
                return Err(fail(
                    "native path derivative crosses a physical box or phase tie",
                ));
            }
        }
        let primal = self.assemble_geometry_path(start, end, design, step_s, boundaries)?;
        if primal.old_geometry.aggregated.active_slots
            != primal.current_geometry.aggregated.active_slots
            || primal.old_geometry.aggregated.component_slots
                != primal.current_geometry.aggregated.component_slots
            || primal.old_geometry.refined.regions != primal.current_geometry.refined.regions
        {
            return Err(fail(
                "native path derivative requires a fixed component stratum",
            ));
        }
        drop(primal);
        let mut values = scope
            .start_columns
            .iter()
            .map(|&i| start[i])
            .chain(scope.end_columns.iter().map(|&i| end[i]))
            .chain(scope.design_columns.iter().map(|&i| design[i]))
            .collect::<Vec<_>>();
        if differentiate_step {
            values.push(step_s);
        }
        let coordinate_scale = owner
            .grid
            .origin_m
            .iter()
            .map(|v| v.abs())
            .fold(owner.grid.spacing_m, f64::max);
        let roundoff = 8192.
            * f64::EPSILON
            * (coordinate_scale / owner.grid.spacing_m).max(1.)
            * (1. + owner.grid.cells.len() as f64);
        for base in (0..values.len()).step_by(16) {
            let mut a = start
                .iter()
                .copied()
                .map(Dual::<16>::constant)
                .collect::<Vec<_>>();
            let mut b = end
                .iter()
                .copied()
                .map(Dual::<16>::constant)
                .collect::<Vec<_>>();
            let mut x = design
                .iter()
                .copied()
                .map(Dual::<16>::constant)
                .collect::<Vec<_>>();
            let mut offset = 0;
            for (destination, columns) in [
                (&mut a, &scope.start_columns),
                (&mut b, &scope.end_columns),
                (&mut x, &scope.design_columns),
            ] {
                for &column in columns {
                    if offset >= base && offset < base + 16 {
                        destination[column].eps[offset - base] = 1.;
                    }
                    offset += 1;
                }
            }
            let mut dt = Dual::<16>::constant(step_s);
            if differentiate_step && offset >= base && offset < base + 16 {
                dt.eps[offset - base] = 1.;
            }
            let _checked = self.assemble_geometry_path(&a, &b, &x, dt, boundaries)?;
            for alpha in [0., 0.5 - 0.5 / 3f64.sqrt(), 0.5 + 0.5 / 3f64.sqrt(), 1.] {
                let trace = a
                    .iter()
                    .zip(&b)
                    .map(|(&a, &b)| a * (1. - alpha) + b * alpha)
                    .collect::<Vec<_>>();
                for defect in self.endpoint(&trace, &x)?.partition_defects_m3 {
                    for (j, &value) in defect.eps.iter().enumerate() {
                        let scale =
                            if base + j < scope.start_columns.len() + scope.end_columns.len() {
                                owner.grid.spacing_m.powi(2)
                            } else {
                                owner.grid.spacing_m.powi(3)
                            };
                        if !value.is_finite() || value.abs() > roundoff * scale {
                            return Err(fail("native path derivative violates capacity partition"));
                        }
                    }
                }
            }
        }
        let gradient = scoped_geometry_pullback(&values, |leaves| {
            let mut a = start
                .iter()
                .copied()
                .map(ReverseScalar::from_f64)
                .collect::<Vec<_>>();
            let mut b = end
                .iter()
                .copied()
                .map(ReverseScalar::from_f64)
                .collect::<Vec<_>>();
            let mut x = design
                .iter()
                .copied()
                .map(ReverseScalar::from_f64)
                .collect::<Vec<_>>();
            let mut offset = 0;
            for (destination, columns) in [
                (&mut a, &scope.start_columns),
                (&mut b, &scope.end_columns),
                (&mut x, &scope.design_columns),
            ] {
                for &column in columns {
                    destination[column] = leaves[offset];
                    offset += 1;
                }
            }
            let dt = if differentiate_step {
                leaves[offset]
            } else {
                ReverseScalar::from_f64(step_s)
            };
            let path = self.assemble_geometry_path(&a, &b, &x, dt, boundaries)?;
            evaluate(&path, &a, &b, &x)
        })?;
        let mut shape = NativeGasShapeBars {
            start: vec![0.; start.len()],
            end: vec![0.; end.len()],
            design: vec![0.; design.len()],
        };
        let mut offset = 0;
        for (destination, columns) in [
            (&mut shape.start, &scope.start_columns),
            (&mut shape.end, &scope.end_columns),
            (&mut shape.design, &scope.design_columns),
        ] {
            for &column in columns {
                destination[column] = gradient[offset];
                offset += 1;
            }
        }
        Ok(NativeGasGeometryPathBars {
            shape,
            step_s: if differentiate_step {
                gradient[offset]
            } else {
                0.
            },
        })
    }
}

#[derive(Clone, Debug)]
pub struct NativeGasMacroGeometryPathBars {
    pub shape: NativeGasShapeBars,
    pub time_scale: f64,
}
impl NativeGasGeometryAdapter {
    pub(crate) fn checked_path_pullback_with_closed_sample(
        &self,
        start: &[f64],
        end: &[f64],
        design: &[f64],
        time_scale: f64,
        nominal_tickstep_s: f64,
        boundaries: &BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
        scope: &NativeGasShapeScope,
        differentiate_time: bool,
        sample: &crate::native_closed_release::NativeClosedReleaseSample,
        evaluate: impl FnOnce(
            &NativeGasGeometryPath<crate::gas_vjp::ReverseScalar>,
            &[crate::gas_vjp::ReverseScalar],
            &[crate::gas_vjp::ReverseScalar],
            &[crate::gas_vjp::ReverseScalar],
        ) -> CaeResult<crate::gas_vjp::ReverseScalar>,
    ) -> CaeResult<NativeGasMacroGeometryPathBars> {
        self.checked_path_pullback_selected_sample(
            start,
            end,
            design,
            time_scale,
            nominal_tickstep_s,
            boundaries,
            scope,
            differentiate_time,
            NativeClosedGeometrySelection::Sample(sample),
            evaluate,
        )
    }
    pub(crate) fn checked_path_pullback_with_closed_trial_sample(
        &self,
        start: &[f64],
        end: &[f64],
        design: &[f64],
        time_scale: f64,
        nominal_tickstep_s: f64,
        boundaries: &BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
        scope: &NativeGasShapeScope,
        differentiate_time: bool,
        sample: &crate::native_closed_release::NativeClosedTrialSample,
        evaluate: impl FnOnce(
            &NativeGasGeometryPath<crate::gas_vjp::ReverseScalar>,
            &[crate::gas_vjp::ReverseScalar],
            &[crate::gas_vjp::ReverseScalar],
            &[crate::gas_vjp::ReverseScalar],
        ) -> CaeResult<crate::gas_vjp::ReverseScalar>,
    ) -> CaeResult<NativeGasMacroGeometryPathBars> {
        self.checked_path_pullback_selected_sample(
            start,
            end,
            design,
            time_scale,
            nominal_tickstep_s,
            boundaries,
            scope,
            differentiate_time,
            NativeClosedGeometrySelection::Trial(sample),
            evaluate,
        )
    }
    fn project_closed_sample_owned<S: GeometryTimeBase>(
        &self,
        start: &[S],
        end: &[S],
        design: &[S],
        time_scale: S,
        selection: NativeClosedGeometrySelection<'_>,
    ) -> CaeResult<(Vec<S>, Vec<S>)> {
        match selection {
            NativeClosedGeometrySelection::Sample(sample) => self
                .project_and_sample_closed_geometry_trace(start, end, design, time_scale, sample),
            NativeClosedGeometrySelection::Trial(sample) => self
                .project_and_sample_closed_trial_geometry_trace(
                    start, end, design, time_scale, sample,
                ),
            _ => Err(fail(
                "macro geometry reverse requires an authenticated sampled owner",
            )),
        }
    }
    pub(crate) fn checked_projected_child_pullback_with_original_macro_receipt<'a>(
        &'a self,
        start: &[f64],
        end: &[f64],
        design: &[f64],
        time_scale: f64,
        scope: &NativeGasShapeScope,
        differentiate_time: bool,
        sample: std::sync::Arc<crate::native_closed_release::NativeClosedReleaseSample>,
        admit: impl Fn(NativeProjectedChildReceipt<'a, implexity_ad::Dual<16>>) -> CaeResult<()>,
        evaluate: impl FnOnce(
            NativeProjectedChildReceipt<'a,crate::gas_vjp::ReverseScalar>,
        ) -> CaeResult<crate::gas_vjp::ReverseScalar>,
    ) -> CaeResult<NativeGasMacroGeometryPathBars> {
        use crate::gas_vjp::{ReverseScalar, scoped_geometry_pullback};
        use implexity_ad::Dual;
        if scope.maximum_owned_cells == 0
            || self.owner.capacity_owner().grid.cells.len() > scope.maximum_owned_cells
        {
            return Err(fail("invalid selected native geometry reverse scope"));
        }
        for (columns, size) in [
            (&scope.start_columns, start.len()),
            (&scope.end_columns, end.len()),
            (&scope.design_columns, design.len()),
        ] {
            let unique: std::collections::BTreeSet<_> = columns.iter().copied().collect();
            if unique.len() != columns.len() || unique.iter().any(|&i| i >= size) {
                return Err(fail("invalid selected native geometry reverse columns"));
            }
        }
        let mut values = Vec::new();
        for (columns, source) in [
            (&scope.start_columns, start),
            (&scope.end_columns, end),
            (&scope.design_columns, design),
        ] {
            values.extend(columns.iter().map(|&i| source[i]));
        }
        if differentiate_time {
            values.push(time_scale);
        }
        if values.is_empty() || values.len() > 48 {
            return Err(fail(
                "selected native geometry reverse requires 1 to 48 original parameters",
            ));
        }
        self.project_and_issue_closed_child(start,end,design,time_scale,sample.clone())?;
        for base in (0..values.len()).step_by(16) {
            let mut a: Vec<_> = start.iter().copied().map(Dual::<16>::constant).collect();
            let mut b: Vec<_> = end.iter().copied().map(Dual::<16>::constant).collect();
            let mut x: Vec<_> = design.iter().copied().map(Dual::<16>::constant).collect();
            let mut time = Dual::<16>::constant(time_scale);
            let mut offset = 0;
            for (destination, columns) in [
                (&mut a, &scope.start_columns),
                (&mut b, &scope.end_columns),
                (&mut x, &scope.design_columns),
            ] {
                for &column in columns {
                    if offset >= base && offset < base + 16 {
                        destination[column].eps[offset - base] = 1.;
                    }
                    offset += 1;
                }
            }
            if differentiate_time && offset >= base && offset < base + 16 {
                time.eps[offset - base] = 1.;
            }
            let receipt=self.project_and_issue_closed_child(&a,&b,&x,time,sample.clone())?;
            let a=receipt.child_start().to_vec();let b=receipt.child_end().to_vec();
            admit(receipt)?;

        }
        let gradient = scoped_geometry_pullback(&values, |leaves| {
            let mut a: Vec<_> = start.iter().copied().map(ReverseScalar::from_f64).collect();
            let mut b: Vec<_> = end.iter().copied().map(ReverseScalar::from_f64).collect();
            let mut x: Vec<_> = design
                .iter()
                .copied()
                .map(ReverseScalar::from_f64)
                .collect();
            let mut offset = 0;
            for (destination, columns) in [
                (&mut a, &scope.start_columns),
                (&mut b, &scope.end_columns),
                (&mut x, &scope.design_columns),
            ] {
                for &column in columns {
                    destination[column] = leaves[offset];
                    offset += 1;
                }
            }
            let time = if differentiate_time {
                leaves[offset]
            } else {
                ReverseScalar::from_f64(time_scale)
            };
            let receipt=self.project_and_issue_closed_child(&a,&b,&x,time,sample.clone())?;
            let a=receipt.child_start().to_vec();let b=receipt.child_end().to_vec();

            evaluate(receipt)
        })?;
        let mut shape = NativeGasShapeBars {
            start: vec![0.; start.len()],
            end: vec![0.; end.len()],
            design: vec![0.; design.len()],
        };
        let mut offset = 0;
        for (destination, columns) in [
            (&mut shape.start, &scope.start_columns),
            (&mut shape.end, &scope.end_columns),
            (&mut shape.design, &scope.design_columns),
        ] {
            for &column in columns {
                destination[column] = gradient[offset];
                offset += 1;
            }
        }
        Ok(NativeGasMacroGeometryPathBars {
            shape,
            time_scale: if differentiate_time {
                gradient[offset]
            } else {
                0.
            },
        })
    }
    pub(crate) fn checked_trial_path_pullback_with_original_macro_receipt<'a>(
        &'a self,
        geometry_owner:&std::sync::Arc<Self>,
        start: &[f64],
        end: &[f64],
        design: &[f64],
        time_scale: f64,
        nominal_tickstep_s: f64,
        boundaries: &BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
        scope: &NativeGasShapeScope,
        differentiate_time: bool,
        sample: std::sync::Arc<crate::native_closed_release::NativeClosedTrialSample>,
        admit: impl Fn(NativeProjectedTrialChildReceipt<'a, implexity_ad::Dual<16>>) -> CaeResult<()>,
        evaluate: impl FnOnce(
            NativeProjectedTrialChildReceipt<'a,crate::gas_vjp::ReverseScalar>,
            &mut NativeGasGeometryPath<crate::gas_vjp::ReverseScalar>,
            &[crate::gas_vjp::ReverseScalar],
            &[crate::gas_vjp::ReverseScalar],
            &[crate::gas_vjp::ReverseScalar],
        ) -> CaeResult<crate::gas_vjp::ReverseScalar>,
    ) -> CaeResult<NativeGasMacroGeometryPathBars> {
        if !std::ptr::eq(self,geometry_owner.as_ref()){return Err(fail("birth reverse adapter owner differs"));}
        use crate::gas_vjp::{ReverseScalar, scoped_geometry_pullback};
        use implexity_ad::Dual;
        let selection=NativeClosedGeometrySelection::Trial(sample.as_ref());
        if scope.maximum_owned_cells == 0
            || self.owner.capacity_owner().grid.cells.len() > scope.maximum_owned_cells
            || !nominal_tickstep_s.is_finite()
            || nominal_tickstep_s <= 0.
        {
            return Err(fail("invalid selected native geometry reverse scope"));
        }
        for (columns, size) in [
            (&scope.start_columns, start.len()),
            (&scope.end_columns, end.len()),
            (&scope.design_columns, design.len()),
        ] {
            let unique: std::collections::BTreeSet<_> = columns.iter().copied().collect();
            if unique.len() != columns.len() || unique.iter().any(|&i| i >= size) {
                return Err(fail("invalid selected native geometry reverse columns"));
            }
        }
        let mut values = Vec::new();
        for (columns, source) in [
            (&scope.start_columns, start),
            (&scope.end_columns, end),
            (&scope.design_columns, design),
        ] {
            values.extend(columns.iter().map(|&i| source[i]));
        }
        if differentiate_time {
            values.push(time_scale);
        }
        if values.is_empty() || values.len() > 48 {
            return Err(fail(
                "selected native geometry reverse requires 1 to 48 original parameters",
            ));
        }
        self.project_closed_sample_owned(start, end, design, time_scale, selection)?;
        for base in (0..values.len()).step_by(16) {
            let mut a: Vec<_> = start.iter().copied().map(Dual::<16>::constant).collect();
            let mut b: Vec<_> = end.iter().copied().map(Dual::<16>::constant).collect();
            let mut x: Vec<_> = design.iter().copied().map(Dual::<16>::constant).collect();
            let mut time = Dual::<16>::constant(time_scale);
            let mut offset = 0;
            for (destination, columns) in [
                (&mut a, &scope.start_columns),
                (&mut b, &scope.end_columns),
                (&mut x, &scope.design_columns),
            ] {
                for &column in columns {
                    if offset >= base && offset < base + 16 {
                        destination[column].eps[offset - base] = 1.;
                    }
                    offset += 1;
                }
            }
            if differentiate_time && offset >= base && offset < base + 16 {
                time.eps[offset - base] = 1.;
            }
            let receipt=self.project_and_issue_closed_trial_child(&a,&b,&x,time,sample.clone())?;
            let a=receipt.child_start().to_vec();let b=receipt.child_end().to_vec();
            admit(receipt)?;
            self.assemble_geometry_path_selected(
                &a,
                &b,
                &x,
                time * nominal_tickstep_s,
                boundaries,
                Some(selection),
                None,
            )?;
        }
        let gradient = scoped_geometry_pullback(&values, |leaves| {
            let mut a: Vec<_> = start.iter().copied().map(ReverseScalar::from_f64).collect();
            let mut b: Vec<_> = end.iter().copied().map(ReverseScalar::from_f64).collect();
            let mut x: Vec<_> = design
                .iter()
                .copied()
                .map(ReverseScalar::from_f64)
                .collect();
            let mut offset = 0;
            for (destination, columns) in [
                (&mut a, &scope.start_columns),
                (&mut b, &scope.end_columns),
                (&mut x, &scope.design_columns),
            ] {
                for &column in columns {
                    destination[column] = leaves[offset];
                    offset += 1;
                }
            }
            let time = if differentiate_time {
                leaves[offset]
            } else {
                ReverseScalar::from_f64(time_scale)
            };
            let receipt=self.project_and_issue_closed_trial_child(&a,&b,&x,time,sample.clone())?;
            let a=receipt.child_start().to_vec();let b=receipt.child_end().to_vec();
            let mut path = self.assemble_geometry_path_selected(
                &a,
                &b,
                &x,
                time * nominal_tickstep_s,
                boundaries,
                Some(selection),
                None,
            )?;
            path.birth_authority=Some(NativeBirthPathAuthority{geometry:geometry_owner.clone(),sample:NativeBirthAuthoritySample::Trial(sample.clone())});
            evaluate(receipt,&mut path, &a, &b, &x)
        })?;
        let mut shape = NativeGasShapeBars {
            start: vec![0.; start.len()],
            end: vec![0.; end.len()],
            design: vec![0.; design.len()],
        };
        let mut offset = 0;
        for (destination, columns) in [
            (&mut shape.start, &scope.start_columns),
            (&mut shape.end, &scope.end_columns),
            (&mut shape.design, &scope.design_columns),
        ] {
            for &column in columns {
                destination[column] = gradient[offset];
                offset += 1;
            }
        }
        Ok(NativeGasMacroGeometryPathBars {
            shape,
            time_scale: if differentiate_time {
                gradient[offset]
            } else {
                0.
            },
        })
    }
    pub(crate) fn checked_path_pullback_with_original_macro_receipt<'a>(
        &'a self,
        geometry_owner:&std::sync::Arc<Self>,
        start: &[f64],
        end: &[f64],
        design: &[f64],
        time_scale: f64,
        nominal_tickstep_s: f64,
        boundaries: &BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
        scope: &NativeGasShapeScope,
        differentiate_time: bool,
        sample: std::sync::Arc<crate::native_closed_release::NativeClosedReleaseSample>,
        admit: impl Fn(NativeProjectedChildReceipt<'a, implexity_ad::Dual<16>>) -> CaeResult<()>,
        evaluate: impl FnOnce(
            NativeProjectedChildReceipt<'a,crate::gas_vjp::ReverseScalar>,
            &mut NativeGasGeometryPath<crate::gas_vjp::ReverseScalar>,
            &[crate::gas_vjp::ReverseScalar],
            &[crate::gas_vjp::ReverseScalar],
            &[crate::gas_vjp::ReverseScalar],
        ) -> CaeResult<crate::gas_vjp::ReverseScalar>,
    ) -> CaeResult<NativeGasMacroGeometryPathBars> {
        if !std::ptr::eq(self,geometry_owner.as_ref()){return Err(fail("birth reverse adapter owner differs"));}
        use crate::gas_vjp::{ReverseScalar, scoped_geometry_pullback};
        use implexity_ad::Dual;
        let selection=NativeClosedGeometrySelection::Sample(sample.as_ref());
        if scope.maximum_owned_cells == 0
            || self.owner.capacity_owner().grid.cells.len() > scope.maximum_owned_cells
            || !nominal_tickstep_s.is_finite()
            || nominal_tickstep_s <= 0.
        {
            return Err(fail("invalid selected native geometry reverse scope"));
        }
        for (columns, size) in [
            (&scope.start_columns, start.len()),
            (&scope.end_columns, end.len()),
            (&scope.design_columns, design.len()),
        ] {
            let unique: std::collections::BTreeSet<_> = columns.iter().copied().collect();
            if unique.len() != columns.len() || unique.iter().any(|&i| i >= size) {
                return Err(fail("invalid selected native geometry reverse columns"));
            }
        }
        let mut values = Vec::new();
        for (columns, source) in [
            (&scope.start_columns, start),
            (&scope.end_columns, end),
            (&scope.design_columns, design),
        ] {
            values.extend(columns.iter().map(|&i| source[i]));
        }
        if differentiate_time {
            values.push(time_scale);
        }
        if values.is_empty() || values.len() > 48 {
            return Err(fail(
                "selected native geometry reverse requires 1 to 48 original parameters",
            ));
        }
        self.project_closed_sample_owned(start, end, design, time_scale, selection)?;
        for base in (0..values.len()).step_by(16) {
            let mut a: Vec<_> = start.iter().copied().map(Dual::<16>::constant).collect();
            let mut b: Vec<_> = end.iter().copied().map(Dual::<16>::constant).collect();
            let mut x: Vec<_> = design.iter().copied().map(Dual::<16>::constant).collect();
            let mut time = Dual::<16>::constant(time_scale);
            let mut offset = 0;
            for (destination, columns) in [
                (&mut a, &scope.start_columns),
                (&mut b, &scope.end_columns),
                (&mut x, &scope.design_columns),
            ] {
                for &column in columns {
                    if offset >= base && offset < base + 16 {
                        destination[column].eps[offset - base] = 1.;
                    }
                    offset += 1;
                }
            }
            if differentiate_time && offset >= base && offset < base + 16 {
                time.eps[offset - base] = 1.;
            }
            let receipt=self.project_and_issue_closed_child(&a,&b,&x,time,sample.clone())?;
            let a=receipt.child_start().to_vec();let b=receipt.child_end().to_vec();
            admit(receipt)?;
            self.assemble_geometry_path_selected(
                &a,
                &b,
                &x,
                time * nominal_tickstep_s,
                boundaries,
                Some(selection),
                None,
            )?;
        }
        let gradient = scoped_geometry_pullback(&values, |leaves| {
            let mut a: Vec<_> = start.iter().copied().map(ReverseScalar::from_f64).collect();
            let mut b: Vec<_> = end.iter().copied().map(ReverseScalar::from_f64).collect();
            let mut x: Vec<_> = design
                .iter()
                .copied()
                .map(ReverseScalar::from_f64)
                .collect();
            let mut offset = 0;
            for (destination, columns) in [
                (&mut a, &scope.start_columns),
                (&mut b, &scope.end_columns),
                (&mut x, &scope.design_columns),
            ] {
                for &column in columns {
                    destination[column] = leaves[offset];
                    offset += 1;
                }
            }
            let time = if differentiate_time {
                leaves[offset]
            } else {
                ReverseScalar::from_f64(time_scale)
            };
            let receipt=self.project_and_issue_closed_child(&a,&b,&x,time,sample.clone())?;
            let a=receipt.child_start().to_vec();let b=receipt.child_end().to_vec();
            let mut path = self.assemble_geometry_path_selected(
                &a,
                &b,
                &x,
                time * nominal_tickstep_s,
                boundaries,
                Some(selection),
                None,
            )?;
            path.birth_authority=Some(NativeBirthPathAuthority{geometry:geometry_owner.clone(),sample:NativeBirthAuthoritySample::Release(sample.clone())});
            evaluate(receipt,&mut path, &a, &b, &x)
        })?;
        let mut shape = NativeGasShapeBars {
            start: vec![0.; start.len()],
            end: vec![0.; end.len()],
            design: vec![0.; design.len()],
        };
        let mut offset = 0;
        for (destination, columns) in [
            (&mut shape.start, &scope.start_columns),
            (&mut shape.end, &scope.end_columns),
            (&mut shape.design, &scope.design_columns),
        ] {
            for &column in columns {
                destination[column] = gradient[offset];
                offset += 1;
            }
        }
        Ok(NativeGasMacroGeometryPathBars {
            shape,
            time_scale: if differentiate_time {
                gradient[offset]
            } else {
                0.
            },
        })
    }
    fn checked_path_pullback_selected_sample(
        &self,
        start: &[f64],
        end: &[f64],
        design: &[f64],
        time_scale: f64,
        nominal_tickstep_s: f64,
        boundaries: &BTreeMap<(Option<usize>, usize, usize), NativeGasExteriorBoundary>,
        scope: &NativeGasShapeScope,
        differentiate_time: bool,
        selection: NativeClosedGeometrySelection<'_>,
        evaluate: impl FnOnce(
            &NativeGasGeometryPath<crate::gas_vjp::ReverseScalar>,
            &[crate::gas_vjp::ReverseScalar],
            &[crate::gas_vjp::ReverseScalar],
            &[crate::gas_vjp::ReverseScalar],
        ) -> CaeResult<crate::gas_vjp::ReverseScalar>,
    ) -> CaeResult<NativeGasMacroGeometryPathBars> {
        use crate::gas_vjp::{ReverseScalar, scoped_geometry_pullback};
        use implexity_ad::Dual;
        if scope.maximum_owned_cells == 0
            || self.owner.capacity_owner().grid.cells.len() > scope.maximum_owned_cells
            || !nominal_tickstep_s.is_finite()
            || nominal_tickstep_s <= 0.
        {
            return Err(fail("invalid selected native geometry reverse scope"));
        }
        for (columns, size) in [
            (&scope.start_columns, start.len()),
            (&scope.end_columns, end.len()),
            (&scope.design_columns, design.len()),
        ] {
            let unique: std::collections::BTreeSet<_> = columns.iter().copied().collect();
            if unique.len() != columns.len() || unique.iter().any(|&i| i >= size) {
                return Err(fail("invalid selected native geometry reverse columns"));
            }
        }
        let mut values = Vec::new();
        for (columns, source) in [
            (&scope.start_columns, start),
            (&scope.end_columns, end),
            (&scope.design_columns, design),
        ] {
            values.extend(columns.iter().map(|&i| source[i]));
        }
        if differentiate_time {
            values.push(time_scale);
        }
        if values.is_empty() || values.len() > 48 {
            return Err(fail(
                "selected native geometry reverse requires 1 to 48 original parameters",
            ));
        }
        self.project_closed_sample_owned(start, end, design, time_scale, selection)?;
        for base in (0..values.len()).step_by(16) {
            let mut a: Vec<_> = start.iter().copied().map(Dual::<16>::constant).collect();
            let mut b: Vec<_> = end.iter().copied().map(Dual::<16>::constant).collect();
            let mut x: Vec<_> = design.iter().copied().map(Dual::<16>::constant).collect();
            let mut time = Dual::<16>::constant(time_scale);
            let mut offset = 0;
            for (destination, columns) in [
                (&mut a, &scope.start_columns),
                (&mut b, &scope.end_columns),
                (&mut x, &scope.design_columns),
            ] {
                for &column in columns {
                    if offset >= base && offset < base + 16 {
                        destination[column].eps[offset - base] = 1.;
                    }
                    offset += 1;
                }
            }
            if differentiate_time && offset >= base && offset < base + 16 {
                time.eps[offset - base] = 1.;
            }
            let (a, b) = self.project_closed_sample_owned(&a, &b, &x, time, selection)?;
            self.assemble_geometry_path_selected(
                &a,
                &b,
                &x,
                time * nominal_tickstep_s,
                boundaries,
                Some(selection),
                None,
            )?;
        }
        let gradient = scoped_geometry_pullback(&values, |leaves| {
            let mut a: Vec<_> = start.iter().copied().map(ReverseScalar::from_f64).collect();
            let mut b: Vec<_> = end.iter().copied().map(ReverseScalar::from_f64).collect();
            let mut x: Vec<_> = design
                .iter()
                .copied()
                .map(ReverseScalar::from_f64)
                .collect();
            let mut offset = 0;
            for (destination, columns) in [
                (&mut a, &scope.start_columns),
                (&mut b, &scope.end_columns),
                (&mut x, &scope.design_columns),
            ] {
                for &column in columns {
                    destination[column] = leaves[offset];
                    offset += 1;
                }
            }
            let time = if differentiate_time {
                leaves[offset]
            } else {
                ReverseScalar::from_f64(time_scale)
            };
            let (a, b) = self.project_closed_sample_owned(&a, &b, &x, time, selection)?;
            let path = self.assemble_geometry_path_selected(
                &a,
                &b,
                &x,
                time * nominal_tickstep_s,
                boundaries,
                Some(selection),
                None,
            )?;
            evaluate(&path, &a, &b, &x)
        })?;
        let mut shape = NativeGasShapeBars {
            start: vec![0.; start.len()],
            end: vec![0.; end.len()],
            design: vec![0.; design.len()],
        };
        let mut offset = 0;
        for (destination, columns) in [
            (&mut shape.start, &scope.start_columns),
            (&mut shape.end, &scope.end_columns),
            (&mut shape.design, &scope.design_columns),
        ] {
            for &column in columns {
                destination[column] = gradient[offset];
                offset += 1;
            }
        }
        Ok(NativeGasMacroGeometryPathBars {
            shape,
            time_scale: if differentiate_time {
                gradient[offset]
            } else {
                0.
            },
        })
    }
}


impl NativeGasGeometryAdapter{
 pub(crate) fn source_wall_construction<S:crate::gas_geometry_time::GeometryTimeBase>(
 &self,support:&NativeGasConvexSupport<crate::gas_geometry_time::GeometryTime<S>>,
 labels:&crate::native_source_stage_labels::PreparedSourceStageLabels<'_,'_,'_,S>,window:usize,
 sample:&crate::native_physical_plane_preparation::PhysicalPlaneSample<crate::gas_geometry_time::GeometryTime<S>,crate::native_source_stage_labels::SourceFaceLabel>,
 current_centroid:[S;3],
 )->CaeResult<crate::native_geometry_construction_error::ConvexSupportConstructionProof>{source_wall_support::issue_source(self,support,labels,window,sample,current_centroid)}
}

impl NativeGasGeometryAdapter{
 pub(crate) fn source_stage_endpoint<S:crate::gas_geometry_time::GeometryTimeBase>(&self,source:&crate::native_source_face_windows::NativeSourceFaceWindows<'_,S>)->CaeResult<NativeGasGeometryEndpoint<crate::gas_geometry_time::GeometryTime<S>>>{if !std::ptr::eq(self,source.adapter()){return Err(fail("source endpoint adapter differs"));}self.endpoint_with_regions(source.live_stage_trace(),source.live_stage_design(),source.stage_tets())}
 pub(crate) fn source_wall_anchor<S:crate::gas_geometry_time::GeometryTimeBase>(
 &self,support:&NativeGasConvexSupport<crate::gas_geometry_time::GeometryTime<S>>,
 labels:&crate::native_source_stage_labels::PreparedSourceStageLabels<'_,'_,'_,S>,window:usize,
 sample:&crate::native_physical_plane_preparation::PhysicalPlaneSample<crate::gas_geometry_time::GeometryTime<S>,crate::native_source_stage_labels::SourceFaceLabel>,
 current_centroid:[S;3],
 )->CaeResult<Option<crate::gas_quadratic_diffuse_wall::QuadraticWallInteriorAnchor<S>>>{
 let token=match &sample.label{crate::native_source_stage_labels::SourceFaceLabel::NativeWallOwned(token)=>token,_=>return Err(fail("source anchor requires owned Native wall"))};
 let(left,component,part,_)=labels.validate_native_wall_owned(token,window,sample.source_uv,sample.position_m,sample.normal,&sample.source_receipt)?;
 if !std::ptr::eq(self,labels.source().adapter())||component!=support.component||part!=support.convex_part{return Err(fail("source anchor selected support differs"));}
 let point=sample.position_m.map(|v|v.value);let normal=sample.normal.map(|v|-v.value);let offset=std::array::from_fn::<_,3,_>(|k|point[k]-current_centroid[k]);
 let distance=(0..3).map(|k|normal[k]*offset[k]).fold(S::zero(),|a,b|a+b);
 if !distance.finite_first_order()||distance.value()==0.{return Err(fail("source anchor route reaches a stratum boundary"));}if distance.value()>0.{return Ok(None);}
 let proof=self.source_wall_construction(support,labels,window,sample,current_centroid)?.value_part();
 let value=support.source_value_part();let bound_position=std::array::from_fn(|k|current_centroid[k]+offset[k]);
 Ok(Some(crate::native_gas_wall_anchor::measured_wall_anchor_with_construction(&value,component,part,left,bound_position,normal,&proof)?))
 }
}
impl<S:crate::gas_geometry_time::GeometryTimeBase> NativeGasConvexSupport<crate::gas_geometry_time::GeometryTime<S>>{
 fn source_value_part(&self)->NativeGasConvexSupport<S>{
 let corner=|c:&NativeCornerConstruction<crate::gas_geometry_time::GeometryTime<S>>|NativeCornerConstruction{cell:c.cell,retained:c.retained,actual:c.actual.map(|v|v.value),active:c.active.clone(),kind:c.kind.clone(),layered_sources:c.layered_sources.clone(),tet_vertices:c.tet_vertices.clone()};
 NativeGasConvexSupport{component:self.component,convex_part:self.convex_part,plane_tet_vertices:self.plane_tet_vertices.clone(),plane_layered_sources:self.plane_layered_sources.clone(),wall_constructions:self.wall_constructions.iter().map(&corner).collect(),corner_constructions:self.corner_constructions.iter().map(|p|p.iter().map(&corner).collect()).collect(),polygons:self.polygons.iter().map(|p|crate::native_cut_cell_partition::CutPolygon{support_only:p.support_only,vertices_m:p.vertices_m.iter().map(|p|p.map(|v|v.value)).collect(),outward_normal:p.outward_normal.map(|v|v.value),plane_origin_m:p.plane_origin_m.map(|v|v.value),plane_offset_m:p.plane_offset_m.value,boundary:p.boundary.clone()}).collect()}
 }
}
