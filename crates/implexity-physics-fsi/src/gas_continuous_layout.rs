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

use crate::gas_quadratic_velocity::QuadraticVelocityBasis;
use crate::gas_transient_kinetic::TransientGasScalar;
use implexity_core::{CaeError, CaeResult};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuousGasRepresentation {
    Kinetic,
    Continuum,
}
#[derive(Clone)]
pub struct ContinuousGasLayout {
    basis: Arc<QuadraticVelocityBasis>,
    representations: Vec<ContinuousGasRepresentation>,
    offsets: Vec<usize>,
}
impl ContinuousGasLayout {
    pub fn new(
        basis: Arc<QuadraticVelocityBasis>,
        representations: Vec<ContinuousGasRepresentation>,
    ) -> CaeResult<Self> {
        if representations.is_empty() {
            return Err(CaeError::contract("continuous gas layout has no slots"));
        }
        let mut offsets = vec![0usize];
        for representation in &representations {
            let count = match representation {
                ContinuousGasRepresentation::Kinetic => basis.coefficient_count(),
                ContinuousGasRepresentation::Continuum => 4,
            };
            offsets.push(offsets.last().unwrap().checked_add(count).ok_or_else(|| {
                CaeError::contract("continuous gas checkpoint dimension overflow")
            })?);
        }
        Ok(Self {
            basis,
            representations,
            offsets,
        })
    }
    pub fn slots(&self) -> usize {
        self.representations.len()
    }
    pub fn state_size(&self) -> usize {
        *self.offsets.last().unwrap()
    }
    pub fn representation(&self, slot: usize) -> CaeResult<ContinuousGasRepresentation> {
        self.representations
            .get(slot)
            .copied()
            .ok_or_else(|| CaeError::contract("continuous gas slot invalid"))
    }
    pub fn range(&self, slot: usize) -> CaeResult<std::ops::Range<usize>> {
        self.representation(slot)?;
        Ok(self.offsets[slot]..self.offsets[slot + 1])
    }
    pub fn basis(&self) -> &Arc<QuadraticVelocityBasis> {
        &self.basis
    }
    pub fn validate<S: TransientGasScalar>(&self, state: &[S], volumes: &[S]) -> CaeResult<()> {
        if state.len() != self.state_size()
            || volumes.len() != self.slots()
            || state.iter().chain(volumes).any(|v| !v.finite_first_order())
        {
            return Err(CaeError::contract(
                "continuous gas checkpoint shape or finite domain",
            ));
        }
        for slot in 0..self.slots() {
            let range = self.range(slot)?;
            if volumes[slot].value() < 0. {
                return Err(CaeError::contract(
                    "continuous gas checkpoint volume negative",
                ));
            }
            match self.representations[slot] {
                ContinuousGasRepresentation::Kinetic => {
                    if state[range.clone()].iter().any(|v| v.value() < 0.) {
                        return Err(CaeError::contract(
                            "continuous gas coefficient inventory negative",
                        ));
                    }
                    if volumes[slot].value() == 0.
                        && (volumes[slot].has_direction()
                            || state[range]
                                .iter()
                                .any(|v| v.value() != 0. || v.has_direction()))
                    {
                        return Err(CaeError::contract(
                            "closed continuous gas slot has inventory or unsupported event direction",
                        ));
                    }
                }
                ContinuousGasRepresentation::Continuum => {
                    if volumes[slot].value() <= 0. || state[range.start].value() <= 0. {
                        return Err(CaeError::contract(
                            "continuum gas requires positive physical volume and mass",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    pub fn macro_inventory<S: TransientGasScalar>(
        &self,
        state: &[S],
        slot: usize,
    ) -> CaeResult<[S; 4]> {
        if state.len() != self.state_size() {
            return Err(CaeError::contract(
                "continuous gas checkpoint shape mismatch",
            ));
        }
        let range = self.range(slot)?;
        match self.representations[slot] {
            ContinuousGasRepresentation::Continuum => {
                Ok(std::array::from_fn(|j| state[range.start + j]))
            }
            ContinuousGasRepresentation::Kinetic => {
                let moments = self.basis.coefficient_moments(&state[range])?;
                Ok(std::array::from_fn(|j| moments[j]))
            }
        }
    }
    pub fn kinetic_inventory<'a, S: TransientGasScalar>(
        &self,
        state: &'a [S],
        slot: usize,
    ) -> CaeResult<&'a [S]> {
        if state.len() != self.state_size()
            || self.representation(slot)? != ContinuousGasRepresentation::Kinetic
        {
            return Err(CaeError::contract(
                "continuous gas kinetic inventory ownership mismatch",
            ));
        }
        Ok(&state[self.range(slot)?])
    }
}
