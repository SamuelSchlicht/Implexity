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

use crate::gas_quadratic_reconstruction::{
    reconstruct_averages_scalar, reconstruct_linear_scalar, QuadraticAverageSample, QuadraticSample,
};
use crate::gas_quadratic_velocity::QuadraticVelocityBasis;
use crate::gas_transient_kinetic::TransientGasScalar;
use implexity_core::{CaeError, CaeResult};
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CoefficientReconstructionOrder {
    Constant,
    Linear,
    Quadratic,
}
#[derive(Clone)]
pub struct CoefficientReconstructionPolicy {
    pub order: CoefficientReconstructionOrder,
    pub axes: [bool; 3],
    pub maximum_condition_number: f64,
}
pub struct CoefficientNeighbor<S> {
    pub centroid_offset_m: [S; 3],
    pub central_covariance_m2: Option<[[S; 3]; 3]>,
    pub average_density: Vec<S>,
}
pub struct CoefficientFaceReconstruction<S> {
    pub density: Vec<S>,
    pub density_gradient: [Vec<S>; 3],
    pub equilibrium_mass_gradient: [S; 3],
    pub equilibrium_momentum_gradient: [[S; 3]; 3],
    pub condition_number: Option<f64>,
    pub fixed_previous_vacuum: bool,
}
fn covariance<S: TransientGasScalar>(c: [[S; 3]; 3]) -> CaeResult<[[S; 3]; 3]> {
    if c.iter().flatten().any(|v| !v.finite_first_order()) {
        return Err(fail("invalid continuous cell central covariance"));
    }
    let scale = c
        .iter()
        .flatten()
        .map(|v| v.value().abs())
        .fold(0., f64::max);
    let direction_scale = c
        .iter()
        .flatten()
        .map(|v| v.maximum_direction())
        .fold(0., f64::max);
    for i in 0..3 {
        for j in 0..3 {
            let d = c[i][j] - c[j][i];
            if d.value().abs() > 256. * f64::EPSILON * scale
                || d.maximum_direction() > 256. * f64::EPSILON * direction_scale
            {
                return Err(fail("continuous cell central covariance is not symmetric"));
            }
        }
    }
    if scale > 0. {
        let a = c.map(|row| row.map(|v| v.value() / scale));
        let tolerance = 256. * f64::EPSILON;
        if (0..3).any(|i| a[i][i] < -tolerance)
            || (0..3)
                .any(|i| ((i + 1)..3).any(|j| a[i][i] * a[j][j] - a[i][j] * a[i][j] < -tolerance))
        {
            return Err(fail(
                "continuous cell central covariance is not positive semidefinite",
            ));
        }
        let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
        if det < -tolerance {
            return Err(fail(
                "continuous cell central covariance is not positive semidefinite",
            ));
        }
    }
    Ok(c)
}
pub fn reconstruct_face<S: TransientGasScalar>(
    basis: &QuadraticVelocityBasis,
    previous_inventory: &[S],
    center_average_density: &[S],
    center_covariance_m2: Option<[[S; 3]; 3]>,
    neighbors: &[CoefficientNeighbor<S>],
    centroid_to_face_m: [S; 3],
    p: &CoefficientReconstructionPolicy,
) -> CaeResult<CoefficientFaceReconstruction<S>> {
    let n = basis.coefficient_count();
    if previous_inventory.len() != n
        || center_average_density.len() != n
        || previous_inventory
            .iter()
            .chain(center_average_density)
            .any(|v| !v.finite_first_order() || v.value() < 0.)
        || centroid_to_face_m.iter().any(|v| !v.finite_first_order())
        || neighbors.iter().any(|s| {
            s.average_density.len() != n
                || s.average_density
                    .iter()
                    .any(|v| !v.finite_first_order() || v.value() < 0.)
                || s.centroid_offset_m.iter().any(|v| !v.finite_first_order())
        })
        || !p.maximum_condition_number.is_finite()
        || p.maximum_condition_number < 1.
    {
        return Err(fail("invalid continuous coefficient reconstruction"));
    }
    let vacuum = previous_inventory.iter().all(|v| v.value() == 0.);
    if vacuum
        && p.order != CoefficientReconstructionOrder::Constant
        && previous_inventory.iter().any(|v| v.has_direction())
    {
        return Err(fail(
            "previous-vacuum reconstruction requires a fixed old-state stratum",
        ));
    }
    if p.order == CoefficientReconstructionOrder::Constant || vacuum {
        return Ok(CoefficientFaceReconstruction {
            density: center_average_density.to_vec(),
            density_gradient: std::array::from_fn(|_| vec![S::zero(); n]),
            equilibrium_mass_gradient: [S::zero(); 3],
            equilibrium_momentum_gradient: [[S::zero(); 3]; 3],
            condition_number: None,
            fixed_previous_vacuum: vacuum,
        });
    }
    let fit = if p.order == CoefficientReconstructionOrder::Quadratic {
        let center = covariance(center_covariance_m2.ok_or_else(|| {
            fail("quadratic coefficient reconstruction needs cell central covariance")
        })?)?;
        let samples = neighbors
            .iter()
            .map(|s| {
                let c = covariance(s.central_covariance_m2.ok_or_else(|| {
                    fail("quadratic coefficient reconstruction needs neighbor central covariance")
                })?)?;
                Ok(QuadraticAverageSample {
                    offset_m: s.centroid_offset_m,
                    covariance_difference_m2: std::array::from_fn(|i| {
                        std::array::from_fn(|j| c[i][j] - center[i][j])
                    }),
                    density_difference_kg_m3: s
                        .average_density
                        .iter()
                        .zip(center_average_density)
                        .map(|(a, b)| *a - *b)
                        .collect(),
                })
            })
            .collect::<CaeResult<Vec<_>>>()?;
        reconstruct_averages_scalar(&samples, p.axes, n, p.maximum_condition_number)?
    } else {
        let samples = neighbors
            .iter()
            .map(|s| QuadraticSample {
                offset_m: s.centroid_offset_m,
                density_difference_kg_m3: s
                    .average_density
                    .iter()
                    .zip(center_average_density)
                    .map(|(a, b)| *a - *b)
                    .collect(),
            })
            .collect::<Vec<_>>();
        reconstruct_linear_scalar(&samples, p.axes, n, p.maximum_condition_number)?
    };
    let center = if p.order == CoefficientReconstructionOrder::Quadratic {
        covariance(center_covariance_m2.unwrap())?
    } else {
        [[S::zero(); 3]; 3]
    };
    let density = (0..n)
        .map(|k| {
            let mut v = center_average_density[k];
            for i in 0..3 {
                v = v + fit.gradient_kg_m4[k][i] * centroid_to_face_m[i];
                for j in 0..3 {
                    v = v + fit.quadratic_kg_m5[k][i][j]
                        * (centroid_to_face_m[i] * centroid_to_face_m[j] - center[i][j]);
                }
            }
            v
        })
        .collect::<Vec<_>>();
    let gradient = std::array::from_fn::<_, 3, _>(|i| {
        (0..n)
            .map(|k| {
                fit.gradient_kg_m4[k][i]
                    + (0..3).fold(S::zero(), |v, j| {
                        v + fit.quadratic_kg_m5[k][i][j] * centroid_to_face_m[j] * 2.
                    })
            })
            .collect::<Vec<_>>()
    });
    if density
        .iter()
        .any(|v| !v.finite_first_order() || v.value() < 0.)
        || gradient.iter().flatten().any(|v| !v.finite_first_order())
    {
        return Err(fail(
            "continuous face coefficient reconstruction is not finite and nonnegative",
        ));
    }
    let mut mass = [S::zero(); 3];
    let mut momentum = [[S::zero(); 3]; 3];
    for k in 0..n {
        let row = basis.integrated_moment_rows()[k];
        for j in 0..3 {
            mass[j] = mass[j] + gradient[j][k] * row[0];
            for i in 0..3 {
                momentum[i][j] = momentum[i][j] + gradient[j][k] * row[i + 1];
            }
        }
    }
    if mass
        .iter()
        .chain(momentum.iter().flatten())
        .any(|v| !v.finite_first_order())
    {
        return Err(fail("continuous face macroscopic gradients are nonfinite"));
    }
    Ok(CoefficientFaceReconstruction {
        density,
        density_gradient: gradient,
        equilibrium_mass_gradient: mass,
        equilibrium_momentum_gradient: momentum,
        condition_number: Some(fit.condition_number),
        fixed_previous_vacuum: false,
    })
}

pub struct CoefficientPointMoments<S> {
    pub moments: [S; 10],
    pub density_gradient: [S; 3],
    pub momentum_gradient: [[S; 3]; 3],
    pub condition_number: Option<f64>,
    pub fixed_previous_vacuum: bool,
}
pub fn reconstruct_point_moments<S: TransientGasScalar>(
    basis: &QuadraticVelocityBasis,
    previous_inventory: &[S],
    center_average_density: &[S],
    center_covariance_m2: Option<[[S; 3]; 3]>,
    neighbors: &[CoefficientNeighbor<S>],
    centroid_to_point_m: [S; 3],
    policy: &CoefficientReconstructionPolicy,
) -> CaeResult<CoefficientPointMoments<S>> {
    let reconstructed = reconstruct_face(
        basis,
        previous_inventory,
        center_average_density,
        center_covariance_m2,
        neighbors,
        centroid_to_point_m,
        policy,
    )?;
    Ok(CoefficientPointMoments {
        moments: basis.coefficient_moments(&reconstructed.density)?,
        density_gradient: reconstructed.equilibrium_mass_gradient,
        momentum_gradient: reconstructed.equilibrium_momentum_gradient,
        condition_number: reconstructed.condition_number,
        fixed_previous_vacuum: reconstructed.fixed_previous_vacuum,
    })
}
