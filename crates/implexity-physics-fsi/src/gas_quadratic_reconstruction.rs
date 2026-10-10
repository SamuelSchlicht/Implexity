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
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
pub struct QuadraticSample<S = f64> {
    pub offset_m: [S; 3],
    pub density_difference_kg_m3: Vec<S>,
}
pub struct QuadraticAverageSample<S = f64> {
    pub offset_m: [S; 3],
    pub covariance_difference_m2: [[S; 3]; 3],
    pub density_difference_kg_m3: Vec<S>,
}
pub struct QuadraticReconstruction<S = f64> {
    pub gradient_kg_m4: Vec<[S; 3]>,
    pub quadratic_kg_m5: Vec<[[S; 3]; 3]>,
    pub condition_number: f64,
}
pub fn reconstruct_scalar<S: TransientGasScalar>(
    samples: &[QuadraticSample<S>],
    axes: [bool; 3],
    population_count: usize,
    maximum_condition_number: f64,
) -> CaeResult<QuadraticReconstruction<S>> {
    reconstruct_with_covariance(
        samples,
        None,
        axes,
        population_count,
        maximum_condition_number,
        true,
    )
}
pub fn reconstruct_averages_scalar<S: TransientGasScalar>(
    samples: &[QuadraticAverageSample<S>],
    axes: [bool; 3],
    population_count: usize,
    maximum_condition_number: f64,
) -> CaeResult<QuadraticReconstruction<S>> {
    let data = samples
        .iter()
        .map(|s| QuadraticSample {
            offset_m: s.offset_m,
            density_difference_kg_m3: s.density_difference_kg_m3.clone(),
        })
        .collect::<Vec<_>>();
    let covariance = samples
        .iter()
        .map(|s| s.covariance_difference_m2)
        .collect::<Vec<_>>();
    reconstruct_with_covariance(
        &data,
        Some(&covariance),
        axes,
        population_count,
        maximum_condition_number,
        true,
    )
}
pub fn reconstruct_linear_scalar<S: TransientGasScalar>(
    samples: &[QuadraticSample<S>],
    axes: [bool; 3],
    population_count: usize,
    maximum_condition_number: f64,
) -> CaeResult<QuadraticReconstruction<S>> {
    reconstruct_with_covariance(
        samples,
        None,
        axes,
        population_count,
        maximum_condition_number,
        false,
    )
}
fn reconstruct_with_covariance<S: TransientGasScalar>(
    samples: &[QuadraticSample<S>],
    covariance: Option<&[[[S; 3]; 3]]>,
    axes: [bool; 3],
    population_count: usize,
    maximum_condition_number: f64,
    quadratic_order: bool,
) -> CaeResult<QuadraticReconstruction<S>> {
    if covariance.is_some_and(|c| {
        c.len() != samples.len()
            || c.iter()
                .flatten()
                .flatten()
                .any(|v| !v.finite_first_order())
    }) {
        return Err(fail("invalid quadratic gas covariance differences"));
    }
    if let Some(c) = covariance {
        for matrix in c {
            let scale = matrix
                .iter()
                .flatten()
                .map(|v| v.value().abs())
                .fold(0., f64::max);
            let direction_scale = matrix
                .iter()
                .flatten()
                .map(|v| v.maximum_direction())
                .fold(0., f64::max);
            for i in 0..3 {
                for j in 0..3 {
                    let d = matrix[i][j] - matrix[j][i];
                    if d.value().abs() > 256. * f64::EPSILON * scale
                        || d.maximum_direction() > 256. * f64::EPSILON * direction_scale
                    {
                        return Err(fail("quadratic gas covariance difference is not symmetric"));
                    }
                }
            }
        }
    }
    let ids = (0..3).filter(|i| axes[*i]).collect::<Vec<_>>();
    let pairs = if quadratic_order {
        ids.iter()
            .enumerate()
            .flat_map(|(k, i)| ids[k..].iter().map(move |j| (*i, *j)))
            .collect::<Vec<_>>()
    } else {
        vec![]
    };
    let n = ids.len() + pairs.len();
    if population_count == 0
        || n == 0
        || samples.len() < n
        || !maximum_condition_number.is_finite()
        || maximum_condition_number < 1.
        || samples.iter().any(|s| {
            s.density_difference_kg_m3.len() != population_count
                || s.offset_m
                    .iter()
                    .chain(&s.density_difference_kg_m3)
                    .any(|v| !v.finite_first_order())
        })
    {
        return Err(fail("invalid quadratic gas reconstruction data"));
    }
    let scales = std::array::from_fn::<_, 3, _>(|i| {
        if axes[i] {
            (samples
                .iter()
                .map(|s| s.offset_m[i] * s.offset_m[i])
                .fold(S::zero(), |a, b| a + b)
                / samples.len() as f64)
                .sqrt()
        } else {
            S::one()
        }
    });
    if scales
        .iter()
        .any(|s| !s.finite_first_order() || s.value() <= 0.)
    {
        return Err(fail(
            "quadratic gas stencil has no extent in an active axis",
        ));
    }
    let mut matrix = vec![vec![S::zero(); n]; n];
    let mut rhs = vec![vec![S::zero(); n]; population_count];
    for (sample_index, sample) in samples.iter().enumerate() {
        let d = std::array::from_fn::<_, 3, _>(|i| sample.offset_m[i] / scales[i]);
        let norm = ids
            .iter()
            .map(|i| d[*i] * d[*i])
            .fold(S::zero(), |a, b| a + b);
        if norm.value() == 0. {
            continue;
        }
        let weight = S::one() / norm;
        let basis = ids
            .iter()
            .map(|i| d[*i])
            .chain(pairs.iter().map(|(i, j)| {
                let v = d[*i] * d[*j];
                if let Some(c) = covariance {
                    v + c[sample_index][*i][*j] / (scales[*i] * scales[*j])
                } else {
                    v
                }
            }))
            .collect::<Vec<_>>();
        for i in 0..n {
            for j in 0..n {
                matrix[i][j] = matrix[i][j] + weight * basis[i] * basis[j];
            }
            for k in 0..population_count {
                rhs[k][i] = rhs[k][i] + weight * basis[i] * sample.density_difference_kg_m3[k];
            }
        }
    }
    let original = matrix.clone();
    let mut inverse = vec![vec![S::zero(); n]; n];
    for i in 0..n {
        inverse[i][i] = S::one();
    }
    for k in 0..n {
        let pivot = (k..n)
            .max_by(|i, j| {
                matrix[*i][k]
                    .value()
                    .abs()
                    .total_cmp(&matrix[*j][k].value().abs())
            })
            .unwrap();
        let scale = matrix[pivot][k];
        if !scale.finite_first_order() || scale.value() == 0. {
            return Err(fail("quadratic gas stencil is rank deficient"));
        }
        matrix.swap(k, pivot);
        inverse.swap(k, pivot);
        for j in 0..n {
            matrix[k][j] = matrix[k][j] / scale;
            inverse[k][j] = inverse[k][j] / scale;
        }
        for i in 0..n {
            if i != k {
                let c = matrix[i][k];
                for j in 0..n {
                    matrix[i][j] = matrix[i][j] - c * matrix[k][j];
                    inverse[i][j] = inverse[i][j] - c * inverse[k][j];
                }
            }
        }
    }
    let norm = |m: &Vec<Vec<S>>| {
        m.iter()
            .map(|r| r.iter().map(|s| s.value().abs()).sum::<f64>())
            .fold(0., f64::max)
    };
    let condition = norm(&original) * norm(&inverse);
    if !condition.is_finite() || condition > maximum_condition_number {
        return Err(fail(
            "quadratic gas stencil exceeds the declared condition limit",
        ));
    }
    let mut gradients = vec![[S::zero(); 3]; population_count];
    let mut quadratic = vec![[[S::zero(); 3]; 3]; population_count];
    for k in 0..population_count {
        let coeff = (0..n)
            .map(|i| {
                (0..n)
                    .map(|j| inverse[i][j] * rhs[k][j])
                    .fold(S::zero(), |a, b| a + b)
            })
            .collect::<Vec<_>>();
        for (d, i) in ids.iter().enumerate() {
            gradients[k][*i] = coeff[d] / scales[*i];
        }
        for (d, (i, j)) in pairs.iter().enumerate() {
            let v = coeff[ids.len() + d] / (scales[*i] * scales[*j]);
            if i == j {
                quadratic[k][*i][*j] = v;
            } else {
                quadratic[k][*i][*j] = v * 0.5;
                quadratic[k][*j][*i] = v * 0.5;
            }
        }
    }
    if gradients
        .iter()
        .flatten()
        .chain(quadratic.iter().flatten().flatten())
        .any(|v| !v.finite_first_order())
    {
        return Err(fail("quadratic gas reconstruction is not finite"));
    }
    Ok(QuadraticReconstruction {
        gradient_kg_m4: gradients,
        quadratic_kg_m5: quadratic,
        condition_number: condition,
    })
}
