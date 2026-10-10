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
    native_cut_cell_partition::CutPolygon, native_cut_cell_refinement::RefinedCutCellPartition,
};
use implexity_core::{CaeError, CaeResult};
use std::collections::BTreeMap;
fn fail(message: &str) -> CaeError {
    CaeError::contract(message)
}
fn inside_one_sided(
    part: &[CutPolygon<f64>],
    point: [f64; 3],
    inward: [f64; 3],
    arithmetic_length_m: f64,
) -> CaeResult<bool> {
    if part.is_empty() {
        return Err(fail("union slot has no supporting planes"));
    }
    for face in part {
        let n = face.outward_normal;
        if n.iter()
            .chain(face.plane_origin_m.iter())
            .any(|v| !v.is_finite())
            || !face.plane_offset_m.is_finite()
        {
            return Err(fail("non-finite union slot plane"));
        }
        let norm = n.iter().map(|v| v * v).sum::<f64>().sqrt();
        if !norm.is_finite() || norm == 0. {
            return Err(fail("invalid union slot plane normal"));
        }
        let terms = std::array::from_fn::<_, 3, _>(|k| n[k] * (point[k] - face.plane_origin_m[k]));
        let distance = terms.iter().copied().sum::<f64>() + face.plane_offset_m;
        let error = arithmetic_length_m * norm
            + 16.
                * f64::EPSILON
                * (terms.iter().map(|v| v.abs()).sum::<f64>() + face.plane_offset_m.abs());
        if !distance.is_finite() || !error.is_finite() {
            return Err(fail("union slot distance overflow"));
        }
        if distance > error {
            return Ok(false);
        }
        if distance < -error {
            continue;
        }
        let terms = std::array::from_fn::<_, 3, _>(|k| n[k] * inward[k]);
        let derivative = terms.iter().copied().sum::<f64>();
        let error = 16. * f64::EPSILON * terms.iter().map(|v| v.abs()).sum::<f64>();
        if !derivative.is_finite() || !error.is_finite() {
            return Err(fail("union slot direction overflow"));
        }
        if derivative > error {
            return Ok(false);
        }
        if derivative != 0. && derivative.abs() <= error {
            return Err(fail("union slot direction sign is unresolved"));
        }
    }
    Ok(true)
}
pub(crate) struct UnionSlotIndex<'a> {
    refined: &'a RefinedCutCellPartition<f64>,
    component_slots: &'a [usize],
    by_cell: BTreeMap<usize, Vec<usize>>,
}
impl<'a> UnionSlotIndex<'a> {
    pub(crate) fn new(
        refined: &'a RefinedCutCellPartition<f64>,
        component_slots: &'a [usize],
    ) -> CaeResult<Self> {
        if component_slots.len() != refined.partition.components.len()
            || !refined.partition.arithmetic_length_m.is_finite()
            || refined.partition.arithmetic_length_m < 0.
        {
            return Err(fail("invalid union slot query"));
        }
        let mut by_cell = BTreeMap::<usize, Vec<usize>>::new();
        for (i, c) in refined.partition.components.iter().enumerate() {
            by_cell.entry(c.cell).or_default().push(i);
        }
        Ok(Self {
            refined,
            component_slots,
            by_cell,
        })
    }
    pub(crate) fn slot_at_face(
        &self,
        cell: usize,
        point: [f64; 3],
        inward: [f64; 3],
    ) -> CaeResult<Option<usize>> {
        self.slot_at_face_filtered(cell,point,inward, None)
    }
    pub(crate) fn slot_at_face_filtered(
        &self,cell:usize,point:[f64;3],inward:[f64;3],
        source:Option<&crate::native_source_stage_labels::SourceAxisSupportQuery<'_>>,
    )->CaeResult<Option<usize>> {
        let refined = self.refined;
        let component_slots = self.component_slots;
        let partition = &refined.partition;
        if component_slots.len() != partition.components.len()
            || !partition.arithmetic_length_m.is_finite()
            || partition.arithmetic_length_m < 0.
            || point.iter().chain(inward.iter()).any(|v| !v.is_finite())
            || inward.iter().all(|v| *v == 0.)
        {
            return Err(fail("invalid union slot query"));
        }
        if !partition.native_capacity_m3.contains_key(&cell) {
            return Err(fail("union slot cell is outside the partition"));
        }
        let mut selected = None;
        for &index in self.by_cell.get(&cell).into_iter().flatten() {
            let component = &partition.components[index];
            let slot = component_slots[index];
            if !component.volume_m3.is_finite() || component.volume_m3 < 0. {
                return Err(fail("invalid union component volume"));
            }
            if component.volume_m3 == 0. {
                continue;
            }
            for part in &component.convex_parts {
                if let Some(query)=source{if query.rejects(self.refined,point,part)?{continue;}}
                if inside_one_sided(part, point, inward, partition.arithmetic_length_m)? {
                    if selected.is_some_and(|previous| previous != slot) {
                        return Err(fail("union face has ambiguous gas slot ownership"));
                    }
                    selected = Some(slot);
                }
            }
        }
        Ok(selected)
    }
    pub(crate) fn component_for_slot_at_face(
        &self, cell: usize, point: [f64; 3], inward: [f64; 3], slot: usize,
    ) -> CaeResult<usize> {
        if self.slot_at_face(cell, point, inward)? != Some(slot) {
            return Err(fail("source wall gas slot differs from partition"));
        }
        let partition = &self.refined.partition;
        for &index in self.by_cell.get(&cell).into_iter().flatten() {
            let component = &partition.components[index];
            if self.component_slots[index] != slot || component.volume_m3 == 0. { continue; }
            for part in &component.convex_parts {
                if inside_one_sided(part, point, inward, partition.arithmetic_length_m)? {
                    return Ok(index);
                }
            }
        }
        Err(fail("source wall has no supporting gas component"))
    }
    pub(crate) fn support_for_slot_at_face_filtered(
        &self,cell:usize,point:[f64;3],inward:[f64;3],slot:usize,
        source:&crate::native_source_stage_labels::SourceAxisSupportQuery<'_>,
        facet:&crate::native_source_stage_labels::SourceNativeFacetIdentity<'_>,
    )->CaeResult<(usize,usize,usize)>{
        if self.slot_at_face_filtered(cell,point,inward,Some(source))?!=Some(slot){return Err(fail("source wall slot differs from its actual partition"));}
        let partition=&self.refined.partition;
        for &index in self.by_cell.get(&cell).into_iter().flatten(){
            let component=&partition.components[index];
            if self.component_slots[index]!=slot||component.volume_m3==0.{continue;}
            for (part_index,part) in component.convex_parts.iter().enumerate(){
                if source.rejects(self.refined,point,part)?{continue;}
                if inside_one_sided(part,point,inward,partition.arithmetic_length_m)?{if let Some(plane)=facet.support_plane_in_cell(self.refined,cell,part)?{return Ok((index,part_index,plane));}}
            }
        }
        Err(fail("source wall has no actual supporting convex part"))
    }
    pub(crate) fn grid_face_slots(
        &self,
        left_cell: usize,
        right_cell: Option<usize>,
        axis: usize,
        normal: f64,
        point: [f64; 3],
    ) -> CaeResult<Option<(usize, Option<usize>)>> {
        if axis >= 3 || (normal != 1. && normal != -1.) || right_cell == Some(left_cell) {
            return Err(fail("invalid union grid face orientation"));
        }
        let mut inward = [0.; 3];
        inward[axis] = -normal;
        let left = self.slot_at_face(left_cell, point, inward)?;
        let right = if let Some(cell) = right_cell {
            inward[axis] = normal;
            self.slot_at_face(cell, point, inward)?
        } else {
            None
        };
        match (left, right, right_cell) {
            (Some(left), Some(right), Some(_)) if left != right => Ok(Some((left, Some(right)))),
            (Some(left), None, None) => Ok(Some((left, None))),
            _ => Ok(None),
        }
    }
}
