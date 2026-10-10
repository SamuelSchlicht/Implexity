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
use crate::gas_velocity_halfspace::HalfspaceMeasure;
use implexity_core::{CaeError, CaeResult};
#[derive(Clone)]
pub struct AxisHalfspaceFactors {
    pub rows: [Vec<Vec<(usize, f64)>>; 3],
    pub dimensions: [usize; 3],
}
fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
pub fn build(
    basis: &QuadraticVelocityBasis,
    normal: [f64; 3],
    offset: f64,
    measure: HalfspaceMeasure,
    coordinate: Option<(usize, f64)>,
) -> CaeResult<Option<AxisHalfspaceFactors>> {
    if normal.iter().chain([&offset]).any(|v| !v.is_finite())
        || coordinate.is_some_and(|(k, v)| k >= 3 || !v.is_finite())
    {
        return Err(fail("invalid axis halfspace parameters"));
    }
    let active: Vec<_> = (0..3).filter(|&k| normal[k] != 0.).collect();
    if active.len() != 1 {
        return Ok(None);
    }
    let axis = active[0];
    let n = normal[axis];
    let root = -offset / n;
    let dimensions = basis.dimensions();
    let mut rows: [Vec<Vec<(usize, f64)>>; 3] =
        std::array::from_fn(|k| vec![vec![]; dimensions[k]]);
    let nodes = [
        -0.8611363115940526,
        -0.3399810435848563,
        0.3399810435848563,
        0.8611363115940526,
    ];
    let weights = [
        0.3478548451374538,
        0.6521451548625461,
        0.6521451548625461,
        0.3478548451374538,
    ];
    for k in 0..3 {
        let knots = basis.knots(k)?;
        let mut band = vec![[0.; 5]; dimensions[k]];
        for segment in 2..knots.len() - 3 {
            let mut lo = knots[segment];
            let mut hi = knots[segment + 1];
            if k == axis {
                if n > 0. {
                    lo = lo.max(root);
                } else {
                    hi = hi.min(root);
                }
            }
            if lo >= hi {
                continue;
            }
            let half = (hi - lo) * 0.5;
            let center = lo + half;
            for q in 0..4 {
                let x = center + half * nodes[q];
                let values = basis.axial_values(k, x)?;
                let mut weight = half * weights[q];
                if k == axis && measure == HalfspaceMeasure::PositiveSpeed {
                    weight *= n * x + offset;
                }
                if let Some((component, ale)) = coordinate {
                    if component == k {
                        weight *= x - ale;
                    }
                }
                if !weight.is_finite() {
                    return Err(fail("axis halfspace integration overflow"));
                }
                for i in 0..dimensions[k] {
                    for j in i.saturating_sub(2)..=i {
                        band[i][j + 2 - i] += weight * values[i] * values[j];
                    }
                }
            }
        }
        for i in 0..dimensions[k] {
            for j in i.saturating_sub(2)..(i + 3).min(dimensions[k]) {
                let value = if i >= j {
                    band[i][j + 2 - i]
                } else {
                    band[j][i + 2 - j]
                };
                if !value.is_finite() {
                    return Err(fail("axis halfspace matrix overflow"));
                }
                if value != 0. {
                    rows[k][i].push((j, value));
                }
            }
        }
    }
    Ok(Some(AxisHalfspaceFactors { rows, dimensions }))
}
impl AxisHalfspaceFactors {
    fn count(&self) -> CaeResult<usize> {
        self.dimensions.iter().try_fold(1usize, |a, &b| {
            a.checked_mul(b)
                .ok_or_else(|| fail("axis halfspace dimension overflow"))
        })
    }
    fn index(&self, c: [usize; 3]) -> usize {
        (c[0] * self.dimensions[1] + c[1]) * self.dimensions[2] + c[2]
    }
    fn action<S: TransientGasScalar>(&self, input: &[S], transpose: bool) -> CaeResult<Vec<S>> {
        if input.len() != self.count()? || input.iter().any(|x| !x.finite_first_order()) {
            return Err(fail("axis halfspace coefficient shape or value invalid"));
        }
        let mut current = input.to_vec();
        for phase in 0..3 {
            let axis = if transpose { 2 - phase } else { phase };
            let mut next = vec![S::zero(); input.len()];
            for x in 0..self.dimensions[0] {
                for y in 0..self.dimensions[1] {
                    for z in 0..self.dimensions[2] {
                        let c = [x, y, z];
                        let i = self.index(c);
                        for &(column, value) in &self.rows[axis][c[axis]] {
                            let mut d = c;
                            d[axis] = column;
                            let j = self.index(d);
                            if transpose {
                                next[j] = next[j] + current[i] * value;
                            } else {
                                next[i] = next[i] + current[j] * value;
                            }
                        }
                    }
                }
            }
            if next.iter().any(|x| !x.finite_first_order()) {
                return Err(fail("axis halfspace action overflow"));
            }
            current = next;
        }
        Ok(current)
    }
    pub fn apply<S: TransientGasScalar>(&self, input: &[S]) -> CaeResult<Vec<S>> {
        self.action(input, false)
    }
    pub fn transpose_apply<S: TransientGasScalar>(&self, input: &[S]) -> CaeResult<Vec<S>> {
        self.action(input, true)
    }
    pub fn materialize_rows<S: TransientGasScalar>(&self) -> Vec<Vec<(usize, S)>> {
        let mut rows = vec![vec![]; self.dimensions.iter().product()];
        for x in 0..self.dimensions[0] {
            for y in 0..self.dimensions[1] {
                for z in 0..self.dimensions[2] {
                    let i = self.index([x, y, z]);
                    for &(a, wa) in &self.rows[0][x] {
                        for &(b, wb) in &self.rows[1][y] {
                            for &(c, wc) in &self.rows[2][z] {
                                let weight = wa * wb * wc;
                                if weight != 0. {
                                    rows[i].push((self.index([a, b, c]), S::from_f64(weight)));
                                }
                            }
                        }
                    }
                }
            }
        }
        rows
    }
    pub fn nonzeros(&self) -> usize {
        self.rows
            .iter()
            .map(|axis| axis.iter().map(Vec::len).sum::<usize>())
            .product()
    }
    pub fn allocated_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .rows
                .iter()
                .map(|a| {
                    a.capacity() * std::mem::size_of::<Vec<(usize, f64)>>()
                        + a.iter()
                            .map(|r| r.capacity() * std::mem::size_of::<(usize, f64)>())
                            .sum::<usize>()
                })
                .sum::<usize>()
    }
}
