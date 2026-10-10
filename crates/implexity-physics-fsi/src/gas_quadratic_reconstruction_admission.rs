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

use crate::gas_quadratic_halfspace::basis_bernstein;
use crate::gas_quadratic_velocity::QuadraticVelocityBasis;
use crate::gas_transient_kinetic::TransientGasScalar;
use implexity_core::{CaeError, CaeResult};
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
struct Piece {
    bounds: [f64; 2],
    active: Vec<(usize, [f64; 3])>,
}
fn pieces(basis: &QuadraticVelocityBasis, axis: usize) -> CaeResult<Vec<Piece>> {
    let knots = basis.knots(axis)?;
    let mut out = Vec::new();
    for w in knots.windows(2) {
        if w[0] == w[1] {
            continue;
        }
        let mut active = Vec::new();
        for i in 0..basis.dimensions()[axis] {
            if knots[i] < w[1] && knots[i + 3] > w[0] {
                let b = basis_bernstein(knots, i, 2, w[0], w[1])?;
                active.push((i, [b[0], b[1], b[2]]));
            }
        }
        out.push(Piece {
            bounds: [w[0], w[1]],
            active,
        });
    }
    Ok(out)
}
pub fn minimum_density<S: TransientGasScalar>(
    basis: &QuadraticVelocityBasis,
    base: &[S],
    gradient: [&[S]; 3],
    ale: [S; 3],
) -> CaeResult<f64> {
    let n = basis.coefficient_count();
    if base.len() != n
        || gradient.iter().any(|v| v.len() != n)
        || base
            .iter()
            .chain(gradient.iter().flat_map(|v| v.iter()))
            .chain(&ale)
            .any(|v| !v.finite_first_order())
    {
        return Err(fail("invalid continuous density polynomial"));
    }
    let axes = [pieces(basis, 0)?, pieces(basis, 1)?, pieces(basis, 2)?];
    let mut minimum = f64::INFINITY;
    for z in &axes[2] {
        for y in &axes[1] {
            for x in &axes[0] {
                let p = [x, y, z];
                let mut b = [[0.; 27]; 4];
                for (iz, bz) in &z.active {
                    for (iy, by) in &y.active {
                        for (ix, bx) in &x.active {
                            let index = basis.tensor_index([*ix, *iy, *iz])?;
                            let c = [
                                base[index].value(),
                                gradient[0][index].value(),
                                gradient[1][index].value(),
                                gradient[2][index].value(),
                            ];
                            for kz in 0..3 {
                                for ky in 0..3 {
                                    for kx in 0..3 {
                                        let q = kx + 3 * (ky + 3 * kz);
                                        let w = bx[kx] * by[ky] * bz[kz];
                                        for j in 0..4 {
                                            b[j][q] += w * c[j];
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                for kz in 0..4 {
                    for ky in 0..4 {
                        for kx in 0..4 {
                            let coordinate = [kx, ky, kz];
                            let mut value = 0.;
                            for qz in 0..3 {
                                for qy in 0..3 {
                                    for qx in 0..3 {
                                        let q = [qx, qy, qz];
                                        let at = qx + 3 * (qy + 3 * qz);
                                        let mut elevation = [0.; 3];
                                        let mut product = [0.; 3];
                                        for j in 0..3 {
                                            let k = coordinate[j];
                                            let i = q[j];
                                            let lower = if i == k && k < 3 {
                                                (3 - k) as f64 / 3.
                                            } else {
                                                0.
                                            };
                                            let upper = if k > 0 && i + 1 == k {
                                                k as f64 / 3.
                                            } else {
                                                0.
                                            };
                                            elevation[j] = lower + upper;
                                            product[j] = lower * (p[j].bounds[0] - ale[j].value())
                                                + upper * (p[j].bounds[1] - ale[j].value());
                                        }
                                        value +=
                                            b[0][at] * elevation[0] * elevation[1] * elevation[2];
                                        for j in 0..3 {
                                            value += b[j + 1][at]
                                                * product[j]
                                                * elevation[(j + 1) % 3]
                                                * elevation[(j + 2) % 3];
                                        }
                                    }
                                }
                            }
                            if !value.is_finite() {
                                return Err(fail(
                                    "continuous density Bernstein certificate is nonfinite",
                                ));
                            }
                            minimum = minimum.min(value);
                        }
                    }
                }
            }
        }
    }
    if !minimum.is_finite() {
        return Err(fail("continuous density certificate is empty"));
    }
    Ok(minimum)
}

#[derive(Default)]
pub struct MinimumDensityCache {
    record: std::sync::Mutex<Option<(Vec<u64>, f64)>>,
}
impl MinimumDensityCache {
    pub fn allocated_bytes(&self) -> CaeResult<usize> {
        let record = self
            .record
            .lock()
            .map_err(|_| fail("density certificate cache poisoned"))?;
        Ok(record
            .as_ref()
            .map_or(0, |(key, _)| key.capacity() * std::mem::size_of::<u64>()))
    }
}
pub fn minimum_density_cached<S: TransientGasScalar>(
    cache: &MinimumDensityCache,
    basis: &QuadraticVelocityBasis,
    base: &[S],
    gradient: [&[S]; 3],
    ale: [S; 3],
) -> CaeResult<f64> {
    let n = basis.coefficient_count();
    if base.len() != n
        || gradient.iter().any(|v| v.len() != n)
        || base
            .iter()
            .chain(gradient.iter().flat_map(|v| v.iter()))
            .chain(&ale)
            .any(|v| !v.finite_first_order())
    {
        return Err(fail("invalid continuous density polynomial"));
    }
    let mut key = Vec::with_capacity(
        4 * n
            + 3
            + basis
                .dimensions()
                .iter()
                .map(|dimension| dimension + 4)
                .sum::<usize>(),
    );
    for axis in 0..3 {
        let knots = basis.knots(axis)?;
        key.push(knots.len() as u64);
        key.extend(knots.iter().map(|v| v.to_bits()));
    }
    key.extend(
        base.iter()
            .chain(gradient.iter().flat_map(|v| v.iter()))
            .chain(&ale)
            .map(|v| v.value().to_bits()),
    );
    {
        let record = cache
            .record
            .lock()
            .map_err(|_| fail("density certificate cache poisoned"))?;
        if let Some((old, minimum)) = record.as_ref() {
            if *old == key {
                return Ok(*minimum);
            }
        }
    }
    let minimum = minimum_density(basis, base, gradient, ale)?;
    *cache
        .record
        .lock()
        .map_err(|_| fail("density certificate cache poisoned"))? = Some((key, minimum));
    Ok(minimum)
}
