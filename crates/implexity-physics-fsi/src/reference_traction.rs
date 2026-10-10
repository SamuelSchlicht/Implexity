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

use crate::gas_transient_kinetic::TransientGasScalar;
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceTractionOwnership {
    #[default]
    PairedPatch,
    GasWettedWalls,
}

impl ReferenceTractionOwnership {
    pub fn patch_pressure_pa<S: Scalar>(
        self,
        reference_pressure_pa: f64,
        corner_gaps_m: [S; 3],
    ) -> CaeResult<f64> {
        if !reference_pressure_pa.is_finite() || reference_pressure_pa < 0. {
            return Err(CaeError::contract("invalid reference pressure"));
        }
        if self == Self::PairedPatch {
            return Ok(-reference_pressure_pa);
        }
        if corner_gaps_m
            .iter()
            .any(|g| !g.value().is_finite() || g.value() < 0.)
        {
            return Err(CaeError::contract(
                "reference ownership requires nonnegative patch gaps",
            ));
        }
        Ok(0.)
    }
}

#[derive(Clone, Debug)]
pub struct WallReferenceAdjustment<S> {
    pub momentum_impulse_kg_m_s: [S; 3],
    pub reference_impulse_kg_m_s: [S; 3],
    pub reference_work_j: S,
}

pub fn adjust_wall_reference<S: TransientGasScalar>(
    absolute_momentum_impulse_kg_m_s: [S; 3],
    outward_area_time_vector_m2_s: [S; 3],
    wall_velocity_m_s: [S; 3],
    reference_pressure_pa: S,
) -> CaeResult<WallReferenceAdjustment<S>> {
    if !reference_pressure_pa.finite_first_order()
        || reference_pressure_pa.value() < 0.
        || absolute_momentum_impulse_kg_m_s
            .iter()
            .chain(&outward_area_time_vector_m2_s)
            .chain(&wall_velocity_m_s)
            .any(|v| !v.finite_first_order())
    {
        return Err(CaeError::contract("invalid wall reference impulse"));
    }
    let reference_impulse_kg_m_s =
        outward_area_time_vector_m2_s.map(|a| -reference_pressure_pa * a);
    let momentum_impulse_kg_m_s = std::array::from_fn(|axis| {
        absolute_momentum_impulse_kg_m_s[axis] + reference_impulse_kg_m_s[axis]
    });
    let reference_work_j = (0..3)
        .map(|axis| reference_impulse_kg_m_s[axis] * wall_velocity_m_s[axis])
        .sum::<S>();
    if momentum_impulse_kg_m_s
        .iter()
        .chain(&reference_impulse_kg_m_s)
        .copied()
        .chain([reference_work_j])
        .any(|v| !v.finite_first_order())
    {
        return Err(CaeError::contract("wall reference impulse overflow"));
    }
    Ok(WallReferenceAdjustment {
        momentum_impulse_kg_m_s,
        reference_impulse_kg_m_s,
        reference_work_j,
    })
}
