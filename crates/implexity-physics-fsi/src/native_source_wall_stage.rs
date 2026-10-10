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

use crate::gas_geometry_time::GeometryTimeBase;
use crate::native_source_stage_labels::{PreparedSourceStageLabels,SourceFaceLabel};
use crate::native_source_face_windows::SourceWindowKind;
use crate::native_cut_cell_partition::CutWallQuadrature;
use crate::native_gas_wall_transfer::NativeGasWallTransfer;
use implexity_core::{CaeError,CaeResult};
use implexity_ad::Scalar;
use std::collections::BTreeMap;
fn fail(s:&str)->CaeError{CaeError::contract(s)}
pub(crate) struct NativeSourceWallStage<S:GeometryTimeBase>{
    pub(crate) quadrature:Vec<CutWallQuadrature<S>>,
    pub(crate) fluid_slots:Vec<usize>,
    pub(crate) convex_supports:Vec<(usize,usize,usize)>,
    pub(crate) transfer:NativeGasWallTransfer<S>,
}
pub(crate) struct NativeSourceWallPreparation<'b,'p,'a,'g,S:GeometryTimeBase>{
    labels:&'b PreparedSourceStageLabels<'p,'a,'g,S>,
}
impl<'b,'p,'a,'g,S:GeometryTimeBase> NativeSourceWallPreparation<'b,'p,'a,'g,S>{
    pub(crate) fn new(labels:&'b PreparedSourceStageLabels<'p,'a,'g,S>)->CaeResult<Self>{
        Ok(Self{labels})
    }
    pub(crate) fn prepare(&self,window:usize,refinement:usize,frame:[f64;2])->CaeResult<NativeSourceWallStage<S>> {
    let labels=self.labels;
    let source=labels.source();
    let face=source.windows().get(window).ok_or_else(||fail("source wall window is out of range"))?;
    if !matches!(face.kind(),SourceWindowKind::NativePhysical){return Err(fail("source wall requires a native physical window"));}
    let samples=labels.prepare_window(window,refinement,frame)?;
    self.prepare_samples(window,&samples)
    }
    pub(crate) fn prepare_samples(&self,window:usize,samples:&[crate::native_physical_plane_preparation::PhysicalPlaneSample<crate::gas_geometry_time::GeometryTime<S>,SourceFaceLabel>])->CaeResult<NativeSourceWallStage<S>>{
    let labels=self.labels;let source=labels.source();
    let face=source.windows().get(window).ok_or_else(||fail("source wall window is out of range"))?;
    if !matches!(face.kind(),SourceWindowKind::NativePhysical){return Err(fail("source wall requires a native physical window"));}
    let owner=source.adapter().owner.capacity_owner();let(body,_element,nodes)=labels.native_wall_parent(window)?;
    let vertices=nodes.map(|i|std::array::from_fn(|k|source.live_stage_trace()[owner.nodes[body][i].force_flux[k]]+owner.nodes[body][i].reference_m[k]));
    let mut quadrature=Vec::new();let mut fluid_slots=Vec::new();let mut convex_supports=Vec::new();
    for sample in samples{
        let (fluid,component,part,plane)=match &sample.label{SourceFaceLabel::NativeWallOwned(token)=>labels.validate_native_wall_owned(token,window,sample.source_uv,sample.position_m,sample.normal,&sample.source_receipt)?,_=>return Err(fail("source wall requires actual per-quadrature support authority"))};
        convex_supports.push((component,part,plane));
        let weights=crate::native_gas_geometry_adapter::material_barycentric(sample.position_m,vertices)?;
        quadrature.push(CutWallQuadrature{wall:quadrature.len(),component,body,native_nodes:nodes,position_m:sample.position_m.map(|v|v.value),native_barycentric:weights.map(|v|v.value),area_m2:sample.area_m2.value,normal:sample.normal.map(|v|-v.value)});
        fluid_slots.push(fluid);
    }
    let trace=source.live_stage_trace().iter().map(|v|v.value).collect::<Vec<_>>();
    let transfer=NativeGasWallTransfer::new(&owner.nodes,&quadrature,&trace,source.adapter().length_tolerance_m)?;
    Ok(NativeSourceWallStage{quadrature,fluid_slots,convex_supports,transfer})
}
}
