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

use super::{aperture_face_bits, aperture_owner_bits, authenticate_convex_aperture, fail, AuthenticatedApertureJets};
use crate::gas_transient_kinetic::TransientGasScalar;
use crate::native_cut_cell_partition::{CutBoundary, CutCellAperture, CutPolygon};
use crate::native_cut_cell_refinement::{ApertureClipSource, CutTetraSubdomain, RefinedCutCellPartition};
use implexity_core::CaeResult;

struct PlaneIndex {
    component: usize,
    part: usize,
    face: usize,
    boundary: CutBoundary,
}
struct PendingRow<S: TransientGasScalar> {
    original: CutCellAperture<S>,
    source: ApertureClipSource<S>,
    planes: Vec<PlaneIndex>,
    subdomains: Vec<(usize, usize)>,
}
pub(super) struct PendingLayeredDispatch<S: TransientGasScalar> {
    rows: Vec<Option<PendingRow<S>>>,
}
struct BoundPlane<S: TransientGasScalar> {
    index: PlaneIndex,
    actual: CutPolygon<S>,
}
struct BoundRow<S: TransientGasScalar> {
    actual: CutCellAperture<S>,
    planes: Vec<BoundPlane<S>>,
    subdomains: Vec<(usize, usize)>,
}
pub(super) struct LayeredDispatch<S: TransientGasScalar> {
    rows: Vec<Option<BoundRow<S>>>,
}
fn plane<'a, S: TransientGasScalar>(refined: &'a RefinedCutCellPartition<S>, index: &PlaneIndex) -> CaeResult<&'a CutPolygon<S>> {
    refined.partition.components.get(index.component).and_then(|c|c.convex_parts.get(index.part)).and_then(|p|p.get(index.face)).ok_or_else(||fail("source dispatch actual supporting face is absent"))
}
fn macro_subdomains<S: TransientGasScalar>(source: &ApertureClipSource<S>, tets: &[CutTetraSubdomain<S>]) -> CaeResult<Vec<(usize,usize)>> {
    let facet=source.macro_facet().ok_or_else(||fail("source dispatch macro owner is absent"))?;
    let faces=[[0,1,2],[0,1,3],[0,2,3],[1,2,3]];
    let mut result=Vec::new();
    for region in [facet.regions.0,facet.regions.1].into_iter().flatten() {
        let mut found=tets.iter().filter(|t|t.region==region);
        let tet=found.next().ok_or_else(||fail("source dispatch macro region is absent"))?;
        if found.next().is_some(){return Err(fail("source dispatch macro region is ambiguous"));}
        let ids=tet.vertex_ids.ok_or_else(||fail("source dispatch macro node identities are absent"))?;
        let face=faces.iter().position(|local|{let mut actual=local.map(|i|ids[i]);actual.sort();actual==facet.vertex_ids}).ok_or_else(||fail("source dispatch macro physical face identities differ"))?;
        result.push((region,face));
    }
    Ok(result)
}
impl<S: TransientGasScalar> PendingLayeredDispatch<S> {
    pub(super) fn issue(refined: &RefinedCutCellPartition<S>, tets: &[CutTetraSubdomain<S>], jets: &AuthenticatedApertureJets<S>) -> CaeResult<Self> {
        if refined.aperture_sources.len()!=refined.partition.apertures.len(){return Err(fail("source dispatch aperture owner count differs"));}
        let mut rows=Vec::with_capacity(refined.partition.apertures.len());
        for(index,actual) in refined.partition.apertures.iter().enumerate() {
            if actual.axis.is_some() || actual.right.is_none(){rows.push(None);continue;}
            let Some(source)=refined.aperture_sources[index].as_ref() else {rows.push(None);continue;};
            let mut planes=Vec::new();
            let subdomains=if source.macro_facet().is_some() {
                if !jets.validate(index,actual)?{return Err(fail("source dispatch macro aperture was not authenticated"));}
                macro_subdomains(source,tets)?
            } else {
                authenticate_convex_aperture(refined,actual,source)?;
                let(left,right,lp,rp,lf,rf)=source.owners();
                if actual.left!=left || actual.right!=Some(right) || actual.polygon_parts!=vec![(lp,Some(rp))]{return Err(fail("source dispatch clip incidence differs"));}
                let mut result=Vec::new();
                for(component,part,face) in [(left,lp,lf),(right,rp,rf)] {
                    let actual_face=refined.partition.components[component].convex_parts[part].get(face).ok_or_else(||fail("source dispatch clip face is absent"))?;
                    if let CutBoundary::Subdomain{region,face}=&actual_face.boundary {result.push((*region,*face));}
                    planes.push(PlaneIndex{component,part,face,boundary:actual_face.boundary.clone()});
                }
                result
            };
            rows.push(Some(PendingRow{original:actual.clone(),source:source.clone(),planes,subdomains}));
        }
        Ok(Self{rows})
    }
    pub(super) fn bind(self, refined: &RefinedCutCellPartition<S>, jets: &AuthenticatedApertureJets<S>) -> CaeResult<LayeredDispatch<S>> {
        if self.rows.len()!=refined.partition.apertures.len() || self.rows.len()!=refined.aperture_sources.len(){return Err(fail("source dispatch reconstructed owner count differs"));}
        let mut rows=Vec::with_capacity(self.rows.len());
        for(index,pending) in self.rows.into_iter().enumerate() {
            let Some(pending)=pending else {rows.push(None);continue;};
            let actual=&refined.partition.apertures[index];
            let source=refined.aperture_sources[index].as_ref().ok_or_else(||fail("source dispatch retained clip owner is absent"))?;
            if !aperture_owner_bits(source.aperture(),&pending.original) || source.owners()!=pending.source.owners() || actual.left!=pending.original.left || actual.right!=pending.original.right || actual.axis!=pending.original.axis || actual.outside_cell!=pending.original.outside_cell || actual.polygon_parts!=pending.original.polygon_parts || actual.polygons.len()!=pending.original.polygons.len(){return Err(fail("source dispatch retained incidence changed during reconstruction"));}
            if source.source().len()!=pending.source.source().len() || source.target().len()!=pending.source.target().len() || !source.source().iter().zip(pending.source.source()).all(|(a,b)|aperture_face_bits(a,b)) || !source.target().iter().zip(pending.source.target()).all(|(a,b)|aperture_face_bits(a,b)){return Err(fail("source dispatch retained source planes changed"));}
            match (source.macro_facet(),pending.source.macro_facet()) {
                (None,None)=>{},
                (Some(a),Some(b))=>{
                    if a.vertex_ids!=b.vertex_ids || a.regions!=b.regions || a.cell!=b.cell || !(0..2).all(|i|(0..3).all(|j|a.bounds[i][j].to_bits()==b.bounds[i][j].to_bits())) || !super::aperture_polygon_bits(&a.triangle,&b.triangle) || a.patches.len()!=b.patches.len() || !a.patches.iter().zip(&b.patches).all(|(a,b)|a.owners()==b.owners() && aperture_owner_bits(a.aperture(),b.aperture())){return Err(fail("source dispatch retained macro source changed"));}
                },
                _=>return Err(fail("source dispatch retained construction kind changed")),
            }
            if pending.source.macro_facet().is_some() && !jets.validate(index,actual)?{return Err(fail("source dispatch reconstructed macro aperture differs"));}
            let mut planes=Vec::new();
            for plane_index in pending.planes {
                let actual_plane=plane(refined,&plane_index)?;
                if actual_plane.boundary!=plane_index.boundary || actual_plane.support_only{return Err(fail("source dispatch reconstructed supporting face changed"));}
                planes.push(BoundPlane{actual:actual_plane.clone(),index:plane_index});
            }
            rows.push(Some(BoundRow{actual:actual.clone(),planes,subdomains:pending.subdomains}));
        }
        Ok(LayeredDispatch{rows})
    }
}
impl<S: TransientGasScalar> LayeredDispatch<S> {
    pub(super) fn subdomains(&self, refined: &RefinedCutCellPartition<S>, index: usize) -> CaeResult<Option<&[(usize,usize)]>> {
        if self.rows.len()!=refined.partition.apertures.len(){return Err(fail("source dispatch live aperture count differs"));}
        let Some(row)=self.rows.get(index).ok_or_else(||fail("source dispatch live aperture index is absent"))?.as_ref() else {return Ok(None);};
        let actual=&refined.partition.apertures[index];
        if !aperture_owner_bits(actual,&row.actual){return Err(fail("source dispatch live aperture polygon or full channels changed"));}
        for bound in &row.planes {
            if !aperture_face_bits(plane(refined,&bound.index)?,&bound.actual){return Err(fail("source dispatch live supporting plane origin offset polygon or full channels changed"));}
        }
        Ok(Some(&row.subdomains))
    }
}
