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

use crate::gas_quadratic_axis_operator::AxisHalfspaceFactors;
use crate::gas_quadratic_velocity::QuadraticVelocityBasis;
use crate::gas_transient_kinetic::TransientGasScalar;
use crate::gas_velocity_halfspace::{HalfspaceMeasure, continuous_piece_bernstein_moments};
use implexity_ad::Dual;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::compact_symmetric::CompactSymmetricRows;
use std::collections::BTreeMap;
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
#[derive(Clone)]
enum OperatorStorage<S> {
    Tensor(std::sync::Arc<AxisHalfspaceFactors>),
    Scalar(std::sync::Arc<Vec<Vec<(usize, S)>>>),
    Constant(std::sync::Arc<CompactSymmetricRows>),
}
#[derive(Clone)]
pub struct QuadraticHalfspaceOperator<S> {
    storage: OperatorStorage<S>,
}
impl<S: TransientGasScalar> QuadraticHalfspaceOperator<S> {
    fn new(rows: Vec<Vec<(usize, S)>>) -> Self {
        if rows.iter().flatten().all(|(_, v)| !v.has_direction()) {
            let upper = rows
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    row.iter()
                        .filter(|(j, _)| *j >= i)
                        .map(|(j, v)| (*j, v.value()))
                        .collect()
                })
                .collect::<Vec<Vec<(usize, f64)>>>();
            if let Ok(packed) = CompactSymmetricRows::new(&upper) {
                return Self {
                    storage: OperatorStorage::Constant(std::sync::Arc::new(packed)),
                };
            }
        }
        Self {
            storage: OperatorStorage::Scalar(std::sync::Arc::new(rows)),
        }
    }
    pub fn constant_lift<T: TransientGasScalar>(&self) -> QuadraticHalfspaceOperator<T> {
        match &self.storage {
            OperatorStorage::Tensor(r) => QuadraticHalfspaceOperator {
                storage: OperatorStorage::Tensor(r.clone()),
            },
            OperatorStorage::Constant(r) => QuadraticHalfspaceOperator {
                storage: OperatorStorage::Constant(r.clone()),
            },
            OperatorStorage::Scalar(r) => QuadraticHalfspaceOperator::new(
                r.iter()
                    .map(|row| {
                        row.iter()
                            .map(|(j, v)| (*j, T::from_f64(v.value())))
                            .collect()
                    })
                    .collect(),
            ),
        }
    }
    pub fn rows(&self) -> std::borrow::Cow<'_, [Vec<(usize, S)>]> {
        match &self.storage {
            OperatorStorage::Tensor(r) => std::borrow::Cow::Owned(r.materialize_rows()),
            OperatorStorage::Scalar(r) => std::borrow::Cow::Borrowed(r.as_slice()),
            OperatorStorage::Constant(r) => {
                let mut rows = vec![vec![]; r.dimension()];
                for i in 0..r.dimension() {
                    r.visit_row(i, |j, value| {
                        rows[i].push((j, S::from_f64(value)));
                        if i != j {
                            rows[j].push((i, S::from_f64(value)));
                        }
                    })
                    .unwrap_or_else(|_| unreachable!());
                }
                for row in &mut rows {
                    row.sort_by_key(|(j, _)| *j);
                }
                std::borrow::Cow::Owned(rows)
            }
        }
    }
    fn dimension(&self) -> usize {
        match &self.storage {
            OperatorStorage::Tensor(r) => r.dimensions.iter().product(),
            OperatorStorage::Scalar(r) => r.len(),
            OperatorStorage::Constant(r) => r.dimension(),
        }
    }
    pub fn apply(&self, coefficients: &[S]) -> CaeResult<Vec<S>> {
        if coefficients.len() != self.dimension()
            || coefficients.iter().any(|v| !v.finite_first_order())
        {
            return Err(fail(
                "quadratic halfspace coefficient shape or value invalid",
            ));
        }
        let out = match &self.storage {
            OperatorStorage::Tensor(r) => r.apply(coefficients)?,
            OperatorStorage::Scalar(rows) => rows
                .iter()
                .map(|r| {
                    r.iter()
                        .fold(S::zero(), |v, (j, a)| v + *a * coefficients[*j])
                })
                .collect::<Vec<_>>(),
            OperatorStorage::Constant(rows) => {
                let mut out = vec![S::zero(); rows.dimension()];
                for i in 0..rows.dimension() {
                    rows.visit_row(i, |j, value| {
                        let a = S::from_f64(value);
                        out[i] = out[i] + a * coefficients[j];
                        if i != j {
                            out[j] = out[j] + a * coefficients[i];
                        }
                    })
                    .map_err(|e| fail(e.to_string()))?;
                }
                out
            }
        };
        if out.iter().any(|v| !v.finite_first_order()) {
            return Err(fail("quadratic halfspace action is nonfinite"));
        }
        Ok(out)
    }
    pub fn transpose_apply(&self, bar: &[S]) -> CaeResult<Vec<S>> {
        if bar.len() != self.dimension() || bar.iter().any(|v| !v.finite_first_order()) {
            return Err(fail("quadratic halfspace cotangent shape or value invalid"));
        }
        let mut out = vec![S::zero(); bar.len()];
        match &self.storage {
            OperatorStorage::Tensor(r) => out = r.transpose_apply(bar)?,
            OperatorStorage::Scalar(rows) => {
                for (i, r) in rows.iter().enumerate() {
                    for (j, a) in r {
                        out[*j] = out[*j] + *a * bar[i];
                    }
                }
            }
            OperatorStorage::Constant(rows) => {
                for i in 0..rows.dimension() {
                    rows.visit_row(i, |j, a| {
                        out[j] = out[j] + S::from_f64(a) * bar[i];
                        if i != j {
                            out[i] = out[i] + S::from_f64(a) * bar[j];
                        }
                    })
                    .map_err(|e| fail(e.to_string()))?;
                }
            }
        }
        if out.iter().any(|v| !v.finite_first_order()) {
            return Err(fail("quadratic halfspace transpose is nonfinite"));
        }
        Ok(out)
    }
    pub fn nonzeros(&self) -> usize {
        match &self.storage {
            OperatorStorage::Tensor(r) => r.nonzeros(),
            OperatorStorage::Scalar(r) => r.iter().map(Vec::len).sum(),
            OperatorStorage::Constant(r) => r.nonzeros(),
        }
    }
    pub fn allocated_storage_bytes(&self) -> usize {
        match &self.storage {
            OperatorStorage::Tensor(r) => r.allocated_bytes(),
            OperatorStorage::Scalar(r) => {
                r.capacity() * std::mem::size_of::<Vec<(usize, S)>>()
                    + r.iter()
                        .map(|r| r.capacity() * std::mem::size_of::<(usize, S)>())
                        .sum::<usize>()
            }
            OperatorStorage::Constant(r) => r.allocated_storage_bytes(),
        }
    }
    pub fn shared_constant_storage(&self) -> Option<usize> {
        match &self.storage {
            OperatorStorage::Tensor(r) => Some(std::sync::Arc::as_ptr(r) as usize),
            OperatorStorage::Constant(r) => Some(std::sync::Arc::as_ptr(r) as usize),
            _ => None,
        }
    }
}

struct Piece {
    lower: f64,
    upper: f64,
    active: Vec<(usize, [f64; 3])>,
}
pub(crate) fn basis_bernstein(
    knots: &[f64],
    index: usize,
    degree: usize,
    lower: f64,
    upper: f64,
) -> CaeResult<Vec<f64>> {
    if degree == 0 {
        return Ok(vec![f64::from(
            knots[index] == lower && knots[index + 1] == upper,
        )]);
    }
    let mut result = vec![0.; degree + 1];
    let terms = [
        (
            index,
            knots[index + degree] - knots[index],
            [lower - knots[index], upper - knots[index]],
        ),
        (
            index + 1,
            knots[index + degree + 1] - knots[index + 1],
            [
                knots[index + degree + 1] - lower,
                knots[index + degree + 1] - upper,
            ],
        ),
    ];
    for (child, denominator, weights) in terms {
        if denominator <= 0. {
            continue;
        }
        let previous = basis_bernstein(knots, child, degree - 1, lower, upper)?;
        if previous.iter().all(|v| *v == 0.) {
            continue;
        }
        if weights.iter().any(|v| !v.is_finite() || *v < 0.) {
            return Err(fail(
                "quadratic Bernstein support has negative recurrence weight",
            ));
        }
        for i in 0..degree {
            for j in 0..2 {
                let factor = if degree == 1 {
                    1.
                } else if i + j == 1 {
                    0.5
                } else {
                    1.
                };
                result[i + j] += previous[i] * weights[j] / denominator * factor;
            }
        }
    }
    Ok(result)
}
fn pieces(basis: &QuadraticVelocityBasis, axis: usize) -> CaeResult<Vec<Piece>> {
    let mut result: Vec<Piece> = vec![];
    for w in basis.knots(axis)?.windows(2) {
        if w[1] > w[0] {
            result.push(Piece {
                lower: w[0],
                upper: w[1],
                active: vec![],
            });
        }
    }
    for i in 0..basis.dimensions()[axis] {
        for part in basis.intervals(axis, i)? {
            let p = result
                .iter_mut()
                .find(|p| p.lower == part.lower_m_s && p.upper == part.upper_m_s)
                .ok_or_else(|| fail("quadratic basis interval is not in knot partition"))?;
            let coefficient = basis_bernstein(basis.knots(axis)?, i, 2, p.lower, p.upper)?;
            p.active
                .push((i, [coefficient[0], coefficient[1], coefficient[2]]));
        }
    }
    if result
        .iter()
        .any(|p| p.active.is_empty() || p.active.len() > 3)
    {
        return Err(fail("quadratic basis interval support invalid"));
    }
    Ok(result)
}
fn products<S: TransientGasScalar>(
    p: &Piece,
    relative_velocity: Option<S>,
) -> Vec<(usize, usize, Vec<S>)> {
    let mut out = vec![];
    for (left, (i, a)) in p.active.iter().enumerate() {
        for (j, b) in &p.active[left..] {
            let mut c = [0.; 5];
            for u in 0..3 {
                for v in 0..3 {
                    let choose2 = [1., 2., 1.];
                    let choose4 = [1., 4., 6., 4., 1.];
                    c[u + v] += a[u] * b[v] * choose2[u] * choose2[v] / choose4[u + v];
                }
            }
            let coefficient = if let Some(relative) = relative_velocity {
                let lo = S::from_f64(p.lower) - relative;
                let hi = S::from_f64(p.upper) - relative;
                let mut result = vec![S::zero(); 6];
                for k in 0..5 {
                    result[k] = result[k] + lo * c[k] * (5 - k) as f64 / 5.;
                    result[k + 1] = result[k + 1] + hi * c[k] * (k + 1) as f64 / 5.;
                }
                result
            } else {
                c.into_iter().map(S::from_f64).collect()
            };
            if i != j {
                out.push((*j, *i, coefficient.clone()));
            }
            out.push((*i, *j, coefficient));
        }
    }
    out
}
pub fn quadratic_halfspace_operator<S: TransientGasScalar>(
    basis: &QuadraticVelocityBasis,
    normal: [S; 3],
    offset: S,
    measure: HalfspaceMeasure,
    maximum_nonzeros: usize,
) -> CaeResult<QuadraticHalfspaceOperator<S>> {
    operator(basis, normal, offset, measure, maximum_nonzeros, None)
}
pub fn quadratic_relative_velocity_halfspace_operator<S: TransientGasScalar>(
    basis: &QuadraticVelocityBasis,
    normal: [S; 3],
    offset: S,
    measure: HalfspaceMeasure,
    component: usize,
    ale_velocity_m_s: S,
    maximum_nonzeros: usize,
) -> CaeResult<QuadraticHalfspaceOperator<S>> {
    if component >= 3 || !ale_velocity_m_s.finite_first_order() {
        return Err(fail("invalid relative velocity halfspace component"));
    }
    operator(
        basis,
        normal,
        offset,
        measure,
        maximum_nonzeros,
        Some((component, ale_velocity_m_s)),
    )
}
fn operator<S: TransientGasScalar>(
    basis: &QuadraticVelocityBasis,
    normal: [S; 3],
    offset: S,
    measure: HalfspaceMeasure,
    maximum_nonzeros: usize,
    relative_velocity: Option<(usize, S)>,
) -> CaeResult<QuadraticHalfspaceOperator<S>> {
    let inputs = [
        normal[0],
        normal[1],
        normal[2],
        offset,
        relative_velocity.map(|(_, v)| v).unwrap_or_else(S::zero),
    ];
    if inputs.iter().any(|v| v.has_direction())
        && inputs
            .iter()
            .any(|v| v.has_direction() && v.first_order_directions().is_none())
    {
        let seeds: [Dual<5>; 5] = std::array::from_fn(|i| {
            let mut eps = [0.; 5];
            eps[i] = 1.;
            Dual {
                re: inputs[i].value(),
                eps,
            }
        });
        let exact = operator(
            basis,
            [seeds[0], seeds[1], seeds[2]],
            seeds[3],
            measure,
            maximum_nonzeros,
            relative_velocity.map(|(j, _)| (j, seeds[4])),
        )?;
        let rows = exact.rows();
        let mut lifted = Vec::with_capacity(rows.len());
        let mut supported = true;
        for row in rows.iter() {
            let mut out = Vec::with_capacity(row.len());
            for (j, v) in row {
                if let Some(value) = S::try_first_order_lift(v.re, &inputs, &v.eps)? {
                    out.push((*j, value));
                } else {
                    supported = false;
                    break;
                }
            }
            if !supported {
                break;
            }
            lifted.push(out);
        }
        if supported {
            return Ok(QuadraticHalfspaceOperator::new(lifted));
        }
    }
    if maximum_nonzeros == 0
        || normal
            .iter()
            .chain(std::iter::once(&offset))
            .any(|v| !v.finite_first_order())
    {
        return Err(fail("invalid quadratic halfspace operator data"));
    }
    if measure == HalfspaceMeasure::Density {
        let active = (0..3)
            .filter(|i| normal[*i].value() != 0.)
            .collect::<Vec<_>>();
        if active.len() == 1 {
            let a = active[0];
            let knots = basis.knots(a)?;
            for edge in [knots[0], knots[knots.len() - 1]] {
                let plane = offset + normal[a] * edge;
                if plane.value() == 0.
                    && (plane.has_direction()
                        || (0..3).any(|i| i != a && normal[i].has_direction()))
                {
                    return Err(fail(
                        "density halfspace derivative reaches the finite velocity-domain boundary",
                    ));
                }
            }
        }
    }
    if normal
        .iter()
        .chain(std::iter::once(&offset))
        .all(|v| !v.has_direction())
        && relative_velocity.is_none_or(|(_, v)| !v.has_direction())
    {
        if let Some(factors) = crate::gas_quadratic_axis_operator::build(
            basis,
            normal.map(|v| v.value()),
            offset.value(),
            measure,
            relative_velocity.map(|(j, v)| (j, v.value())),
        )? {
            if factors.nonzeros() > maximum_nonzeros {
                return Err(fail(
                    "quadratic halfspace operator exceeds its nonzero budget",
                ));
            }
            return Ok(QuadraticHalfspaceOperator {
                storage: OperatorStorage::Tensor(std::sync::Arc::new(factors)),
            });
        }
    }
    let axis = [pieces(basis, 0)?, pieces(basis, 1)?, pieces(basis, 2)?];
    let mut rows = vec![BTreeMap::<usize, S>::new(); basis.coefficient_count()];
    let mut nonzeros = 0;
    for x in &axis[0] {
        for y in &axis[1] {
            for z in &axis[2] {
                let m = continuous_piece_bernstein_moments(
                    [[x.lower, x.upper], [y.lower, y.upper], [z.lower, z.upper]],
                    normal,
                    offset,
                    std::array::from_fn(|i| {
                        if relative_velocity.is_some_and(|(axis, _)| axis == i) {
                            5
                        } else {
                            4
                        }
                    }),
                    measure,
                )?;
                if m.moments
                    .iter()
                    .all(|v| v.value() == 0. && !v.has_direction())
                {
                    continue;
                }
                let axis_velocity = |i| {
                    relative_velocity
                        .filter(|(axis, _)| *axis == i)
                        .map(|(_, v)| v)
                };
                let px = products(x, axis_velocity(0));
                let py = products(y, axis_velocity(1));
                let pz = products(z, axis_velocity(2));
                let nx = m.degrees[0] + 1;
                let ny = m.degrees[1] + 1;
                let nz = m.degrees[2] + 1;
                let mut first = vec![vec![vec![S::zero(); nz]; ny]; px.len()];
                for (r, (_, _, c)) in px.iter().enumerate() {
                    for j in 0..ny {
                        for k in 0..nz {
                            for i in 0..nx {
                                first[r][j][k] =
                                    first[r][j][k] + m.moments[(i * ny + j) * nz + k] * c[i];
                            }
                        }
                    }
                }
                let mut second = vec![vec![S::zero(); nz]; px.len() * py.len()];
                for r in 0..px.len() {
                    for (s, (_, _, c)) in py.iter().enumerate() {
                        for k in 0..nz {
                            for j in 0..ny {
                                second[r * py.len() + s][k] =
                                    second[r * py.len() + s][k] + first[r][j][k] * c[j];
                            }
                        }
                    }
                }
                for (r, (xi, xj, _)) in px.iter().enumerate() {
                    for (s, (yi, yj, _)) in py.iter().enumerate() {
                        for (zi, zj, c) in &pz {
                            let value = (0..nz)
                                .fold(S::zero(), |v, k| v + second[r * py.len() + s][k] * c[k]);
                            if !value.finite_first_order() {
                                return Err(fail("nonfinite quadratic halfspace matrix entry"));
                            }
                            if value.value() == 0. && !value.has_direction() {
                                continue;
                            }
                            let i = basis.tensor_index([*xi, *yi, *zi])?;
                            let j = basis.tensor_index([*xj, *yj, *zj])?;
                            if !rows[i].contains_key(&j) {
                                nonzeros += 1;
                                if nonzeros > maximum_nonzeros {
                                    return Err(fail(
                                        "quadratic halfspace sparse matrix exceeds declared cost bound",
                                    ));
                                }
                            }
                            let entry = rows[i].entry(j).or_insert(S::zero());
                            *entry = *entry + value;
                        }
                    }
                }
            }
        }
    }
    let mut out = vec![];
    for row in rows {
        let r = row.into_iter().collect::<Vec<_>>();
        if r.iter().any(|(_, v)| {
            !v.finite_first_order() || (relative_velocity.is_none() && v.value() < 0.)
        }) {
            return Err(fail(
                "quadratic halfspace matrix lost nonnegative polynomial integration",
            ));
        }
        out.push(r);
    }
    Ok(QuadraticHalfspaceOperator::new(out))
}
