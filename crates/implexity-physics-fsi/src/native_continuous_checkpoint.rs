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

use crate::gas_continuous_layout::ContinuousGasLayout;
use crate::gas_transient_kinetic::TransientGasScalar;
use implexity_core::{CaeError, CaeResult};
use std::{ops::Range, sync::Arc};

#[derive(Clone)]
pub struct NativeContinuousCheckpointLayout {
    gas: Arc<ContinuousGasLayout>,
    bulk: Range<usize>,
    gas_range: Range<usize>,
    stress: Range<usize>,
}
pub struct NativeContinuousCheckpointView<'a, S> {
    pub bulk: &'a [S],
    pub gas: &'a [S],
    pub stress_trace_pa: &'a [S],
}
impl NativeContinuousCheckpointLayout {
    pub fn new(
        bulk_size: usize,
        gas: Arc<ContinuousGasLayout>,
        port_count: usize,
    ) -> CaeResult<Self> {
        if bulk_size == 0 || port_count == 0 {
            return Err(CaeError::contract(
                "native continuous checkpoint requires bulk and ports",
            ));
        }
        let gas_end = bulk_size
            .checked_add(gas.state_size())
            .ok_or_else(|| CaeError::contract("native continuous checkpoint dimension overflow"))?;
        let stress_count = port_count
            .checked_mul(6)
            .ok_or_else(|| CaeError::contract("native continuous stress dimension overflow"))?;
        let end = gas_end
            .checked_add(stress_count)
            .ok_or_else(|| CaeError::contract("native continuous checkpoint dimension overflow"))?;
        Ok(Self {
            gas,
            bulk: 0..bulk_size,
            gas_range: bulk_size..gas_end,
            stress: gas_end..end,
        })
    }
    pub fn dimension(&self) -> usize {
        self.stress.end
    }
    pub fn bulk_range(&self) -> Range<usize> {
        self.bulk.clone()
    }
    pub fn gas_range(&self) -> Range<usize> {
        self.gas_range.clone()
    }
    pub fn stress_range(&self) -> Range<usize> {
        self.stress.clone()
    }
    pub fn gas_layout(&self) -> &Arc<ContinuousGasLayout> {
        &self.gas
    }
    pub fn split<'a, S: TransientGasScalar>(
        &self,
        state: &'a [S],
    ) -> CaeResult<NativeContinuousCheckpointView<'a, S>> {
        if state.len() != self.dimension() || state.iter().any(|v| !v.finite_first_order()) {
            return Err(CaeError::contract(
                "native continuous checkpoint shape or finite domain",
            ));
        }
        Ok(NativeContinuousCheckpointView {
            bulk: &state[self.bulk.clone()],
            gas: &state[self.gas_range.clone()],
            stress_trace_pa: &state[self.stress.clone()],
        })
    }
    pub fn pack<S: TransientGasScalar>(
        &self,
        bulk: &[S],
        gas: &[S],
        stress_trace_pa: &[S],
    ) -> CaeResult<Vec<S>> {
        if bulk.len() != self.bulk.len()
            || gas.len() != self.gas_range.len()
            || stress_trace_pa.len() != self.stress.len()
            || bulk
                .iter()
                .chain(gas)
                .chain(stress_trace_pa)
                .any(|v| !v.finite_first_order())
        {
            return Err(CaeError::contract(
                "native continuous checkpoint component shape or finite domain",
            ));
        }
        let mut state = Vec::with_capacity(self.dimension());
        state.extend_from_slice(bulk);
        state.extend_from_slice(gas);
        state.extend_from_slice(stress_trace_pa);
        Ok(state)
    }
}
