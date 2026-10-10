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
use crate::native_cut_cell_partition::CutBoundary;
use crate::native_normal_birth::NativeNormalBirthStage;
use implexity_core::{CaeError,CaeResult};
use std::{collections::BTreeMap,sync::Arc};

impl NativeContinuousCoupledField {
    pub(crate) fn birth_stages_for_runtime_sample<'a,S:GeometryTimeBase>(
        &'a self,macro_index:usize,tick:usize,macro_start:&[S],macro_end:&[S],
        design:&[S],time_scale:S,sample:Arc<NativeClosedReleaseSample>,
        native_facets:&BTreeMap<usize,[CutBoundary;2]>,
    )->CaeResult<Vec<NativeNormalBirthStage<'a,S>>> {
        let projected=self.config.geometry.project_and_issue_closed_child(macro_start,macro_end,design,time_scale,sample)?;
        self.birth_stages_for_runtime_receipt(macro_index,tick,projected,native_facets)
    }
    pub(crate) fn birth_stages_for_runtime_receipt<'a,S:GeometryTimeBase>(
        &'a self,macro_index:usize,tick:usize,
        projected:crate::native_gas_geometry_adapter::NativeProjectedChildReceipt<'a,S>,
        native_facets:&BTreeMap<usize,[CutBoundary;2]>,
    )->CaeResult<Vec<NativeNormalBirthStage<'a,S>>> {
        let sample=projected.sample().clone();
        let charts=sample.closed_start_charts().iter().copied()
            .filter(|chart|!sample.closed_end_charts().contains(chart)).collect::<Vec<_>>();
        if charts.iter().copied().ne(native_facets.keys().copied()) {
            return Err(CaeError::contract("runtime birth facets do not cover the released charts"));
        }
        let clock=self.birth_clock_for_tick(macro_index,tick,projected.design(),projected.time_scale(),sample.clone())?;
        let adapter=self.config.geometry.as_ref();
        if charts.is_empty() { return Ok(Vec::new()); }
        let first=charts[0];
        let stage=NativeNormalBirthStage::from_runtime_projected_child(clock,projected,first,native_facets[&first].clone())?;
        let context=stage.context()?.clone();
        let mut stages=vec![stage];
        for &chart in &charts[1..] {
            stages.push(NativeNormalBirthStage::from_shared_context(
                &adapter.kinematics,adapter.owner.capacity_owner(),context.clone(),chart,native_facets[&chart].clone(),
            )?);
        }
        Ok(stages)
    }
}
