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
use crate::native_closed_release::NativeClosedReleaseSample;
use crate::native_continuous_coupled_field::NativeContinuousCoupledField;
use crate::native_gas_geometry_adapter::{NativeGasGeometryAdapter, NativeProjectedChildReceipt};
use implexity_core::{CaeError,CaeResult};
use implexity_solve::time_stepper::StepParameters;
use std::sync::Arc;
fn fail(s:&str)->CaeError { CaeError::contract(s) }
pub(crate) struct NativeRuntimeBirthClock<'a,S:GeometryTimeBase> {
    field:&'a NativeContinuousCoupledField,
    sample:Arc<NativeClosedReleaseSample>,
    macro_index:usize,
    tick:usize,
    nominal_step_s:f64,
    time_scale:S,
}
impl<'a,S:GeometryTimeBase> NativeRuntimeBirthClock<'a,S> {
    pub(crate) fn adapter(&self)->&'a NativeGasGeometryAdapter { self.field.config.geometry.as_ref() }
    pub(crate) fn nominal_step_s(&self)->f64 { self.nominal_step_s }
    pub(crate) fn physical_step_s(&self)->S { S::from_f64(self.nominal_step_s)*self.time_scale }
    pub(crate) fn macro_index(&self)->usize { self.macro_index }
    pub(crate) fn tick(&self)->usize { self.tick }
    pub(crate) fn validate_receipt(&self,receipt:&NativeProjectedChildReceipt<'_,S>)->CaeResult<()> {
        if !std::ptr::eq(self.adapter(),receipt.adapter()) || !Arc::ptr_eq(&self.sample,receipt.sample()) {
            return Err(fail("birth clock and projected child have different runtime owners"));
        }
        let a=self.time_scale;let b=receipt.time_scale();
        if a.value().to_bits()!=b.value().to_bits() || match(a.geometry_first_order_directions(),b.geometry_first_order_directions()) {
            (Some(a),Some(b))=>a.iter().map(|v|v.to_bits()).ne(b.iter().map(|v|v.to_bits())),
            _=>a.has_direction()||b.has_direction(),
        } { return Err(fail("birth clock scalar time leaves differ")); }
        Ok(())
    }
}
impl NativeContinuousCoupledField {
    pub(crate) fn birth_clock_for_tick<'a,S:GeometryTimeBase>(
        &'a self,macro_index:usize,tick:usize,design:&[S],time_scale:S,
        sample:Arc<NativeClosedReleaseSample>,
    )->CaeResult<NativeRuntimeBirthClock<'a,S>> {
        let cfg=self.config.bulk.config();
        if tick==0 || tick>cfg.substeps || time_scale.value()!=1. || !time_scale.finite_first_order()
            || time_scale.geometry_first_order_directions().map_or(time_scale.has_direction(),|d|d.iter().any(|v|*v!=0.)) {
            return Err(fail("birth clock is outside the fixed native runtime schedule"));
        }
        let expected=((tick-1) as f64/cfg.substeps as f64,tick as f64/cfg.substeps as f64);
        let actual=sample.fractions();
        if actual.0.to_bits()!=expected.0.to_bits() || actual.1.to_bits()!=expected.1.to_bits() {
            return Err(fail("birth sample fractions differ from the native tick schedule"));
        }
        let design=design.iter().map(|v|v.value()).collect::<Vec<_>>();
        sample.parent().validate_macro(macro_index,StepParameters{design:&design,time_scale:time_scale.value()})?;
        Ok(NativeRuntimeBirthClock{field:self,sample,macro_index,tick,nominal_step_s:cfg.macro_step_s/cfg.substeps as f64,time_scale})
    }
}
