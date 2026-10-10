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

use crate::moving_contact::contact_field::ContactContribution;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Copy, Debug)]
pub struct ContactBranch {
    pub gap_m: f64,
    pub multiplier_n: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactOriginSelection {
    Closed,
    Open,
}
pub fn validate_contact_origin_direction(
    branches: &[ContactBranch],
    selections: &BTreeMap<usize, ContactOriginSelection>,
    gap_directions_m: &[f64],
    multiplier_directions_n: &[f64],
    gap_roundoff_m: f64,
    force_roundoff_n: f64,
) -> CaeResult<()> {
    if branches.len() != gap_directions_m.len()
        || branches.len() != multiplier_directions_n.len()
        || !gap_roundoff_m.is_finite()
        || gap_roundoff_m < 0.
        || !force_roundoff_n.is_finite()
        || force_roundoff_n < 0.
        || gap_directions_m
            .iter()
            .chain(multiplier_directions_n)
            .any(|v| !v.is_finite())
        || selections.keys().any(|&i| i >= branches.len())
    {
        return Err(fail("invalid contact origin direction admission"));
    }
    for (i, b) in branches.iter().enumerate() {
        let biactive = b.gap_m == 0. && b.multiplier_n == 0.;
        if !b.gap_m.is_finite()
            || !b.multiplier_n.is_finite()
            || b.gap_m < 0.
            || b.multiplier_n < 0.
            || b.gap_m > 0. && b.multiplier_n > 0.
        {
            return Err(fail("invalid contact origin branch"));
        }
        match (biactive, selections.get(&i)) {
            (true, Some(ContactOriginSelection::Closed)) => {
                if gap_directions_m[i].abs() > gap_roundoff_m
                    || multiplier_directions_n[i] < -force_roundoff_n
                {
                    return Err(fail("closed contact origin predictor leaves its cone"));
                }
            }
            (true, Some(ContactOriginSelection::Open)) => {
                if multiplier_directions_n[i].abs() > force_roundoff_n
                    || gap_directions_m[i] < -gap_roundoff_m
                {
                    return Err(fail("open contact origin predictor leaves its cone"));
                }
            }
            (true, None) => return Err(fail("biactive contact has no selected predictor branch")),
            (false, Some(_)) => return Err(fail("contact origin selection is stale")),
            _ => {}
        }
    }
    Ok(())
}
#[derive(Clone, Copy, Debug)]
pub struct ContactTangentPolicy {
    pub rank_relative_tolerance: f64,
    pub compatibility_relative_tolerance: f64,
}
#[derive(Clone, Debug)]
struct Row {
    current: BTreeMap<usize, f64>,
    affine: BTreeMap<usize, f64>,
    source: usize,
    affine_scale: f64,
}
#[derive(Clone, Debug)]
pub struct ContactConstraintTangent {
    columns: Vec<usize>,
    current_size: usize,
    previous_size: usize,
    design_size: usize,
    rows: Vec<Row>,
    pub biactive_closed_rows: Vec<usize>,
    pub biactive_open_rows: Vec<usize>,
    pub independent_rows: Vec<usize>,
    pub redundant_rows: Vec<usize>,
    pub maximum_redundant_current_norm: f64,
    pub maximum_redundant_affine_norm: f64,
}
#[derive(Clone, Debug)]
pub struct ContactTangentBars {
    pub current: Vec<f64>,
    pub previous: Vec<f64>,
    pub design: Vec<f64>,
    pub time: f64,
}
fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
fn norm(x: &BTreeMap<usize, f64>) -> f64 {
    x.values().fold(0., |a, &b| a.hypot(b))
}
fn dot(a: &BTreeMap<usize, f64>, b: &BTreeMap<usize, f64>) -> f64 {
    a.iter()
        .map(|(i, v)| v * b.get(i).copied().unwrap_or(0.))
        .sum()
}
fn subtract(a: &mut BTreeMap<usize, f64>, b: &BTreeMap<usize, f64>, scale: f64) {
    for (&i, &v) in b {
        let x = a.entry(i).or_default();
        *x -= scale * v;
    }
    a.retain(|_, v| *v != 0.);
}
fn finite(row: &Row) -> bool {
    row.current
        .values()
        .chain(row.affine.values())
        .all(|v| v.is_finite())
}
impl ContactConstraintTangent {
    pub fn from_contact(
        c: &ContactContribution,
        columns: Vec<usize>,
        branches: &[ContactBranch],
        policy: ContactTangentPolicy,
    ) -> CaeResult<Self> {
        Self::from_rows(
            &c.constraint_current.to_csr()?,
            &c.constraint_previous.to_csr()?,
            &c.constraint_design.to_csr()?,
            &c.constraint_time,
            columns,
            branches,
            policy,
        )
    }
    pub fn from_rows(
        current: &CsrMatrix,
        previous: &CsrMatrix,
        design: &CsrMatrix,
        time: &[f64],
        columns: Vec<usize>,
        branches: &[ContactBranch],
        policy: ContactTangentPolicy,
    ) -> CaeResult<Self> {
        Self::from_rows_with_origins(
            current,
            previous,
            design,
            time,
            columns,
            branches,
            &BTreeMap::new(),
            policy,
        )
    }
    pub fn from_contact_with_origins(
        c: &ContactContribution,
        columns: Vec<usize>,
        branches: &[ContactBranch],
        origins: &BTreeMap<usize, ContactOriginSelection>,
        policy: ContactTangentPolicy,
    ) -> CaeResult<Self> {
        Self::from_rows_with_origins(
            &c.constraint_current.to_csr()?,
            &c.constraint_previous.to_csr()?,
            &c.constraint_design.to_csr()?,
            &c.constraint_time,
            columns,
            branches,
            origins,
            policy,
        )
    }
    pub fn from_rows_with_origins(
        current: &CsrMatrix,
        previous: &CsrMatrix,
        design: &CsrMatrix,
        time: &[f64],
        columns: Vec<usize>,
        branches: &[ContactBranch],
        origins: &BTreeMap<usize, ContactOriginSelection>,
        policy: ContactTangentPolicy,
    ) -> CaeResult<Self> {
        if !policy.rank_relative_tolerance.is_finite()
            || policy.rank_relative_tolerance < 32. * f64::EPSILON
            || policy.rank_relative_tolerance >= 1.
            || !policy.compatibility_relative_tolerance.is_finite()
            || policy.compatibility_relative_tolerance < 32. * f64::EPSILON
            || policy.compatibility_relative_tolerance >= 1.
            || current.nrows() != previous.nrows()
            || current.nrows() != design.nrows()
            || current.nrows() != time.len()
            || current.nrows() != branches.len()
            || columns.iter().any(|&i| i >= current.ncols())
            || columns.iter().copied().collect::<BTreeSet<_>>().len() != columns.len()
        {
            return Err(fail("invalid contact tangent shape or rank policy"));
        }
        if origins.keys().any(|&i| i >= branches.len()) {
            return Err(fail("contact origin row is outside native constraints"));
        }
        let mut biactive_closed_rows = vec![];
        let mut biactive_open_rows = vec![];
        let mut selected = Vec::new();
        for (i, branch) in branches.iter().enumerate() {
            if !branch.gap_m.is_finite()
                || !branch.multiplier_n.is_finite()
                || branch.gap_m < 0.
                || branch.multiplier_n < 0.
                || branch.gap_m > 0. && branch.multiplier_n > 0.
            {
                return Err(fail("invalid contact complementarity branch"));
            }
            if branch.gap_m == 0. && branch.multiplier_n == 0. {
                match origins.get(&i) {
                    Some(ContactOriginSelection::Closed) => biactive_closed_rows.push(i),
                    Some(ContactOriginSelection::Open) => {
                        biactive_open_rows.push(i);
                        continue;
                    }
                    None => return Err(fail("biactive contact needs an event derivative owner")),
                }
            } else if origins.contains_key(&i) {
                return Err(fail("contact origin selection is stale"));
            }
            if branch.gap_m > 0. {
                continue;
            }
            selected.push(i);
        }
        Self::build_selected_rows(
            current,
            previous,
            design,
            time,
            columns,
            &selected,
            biactive_closed_rows,
            biactive_open_rows,
            policy,
        )
    }
    pub(crate) fn from_selected_rows(
        current: &CsrMatrix,
        previous: &CsrMatrix,
        design: &CsrMatrix,
        time: &[f64],
        columns: Vec<usize>,
        selected: &[usize],
        policy: ContactTangentPolicy,
    ) -> CaeResult<Self> {
        Self::build_selected_rows(
            current,
            previous,
            design,
            time,
            columns,
            selected,
            vec![],
            vec![],
            policy,
        )
    }
    fn build_selected_rows(
        current: &CsrMatrix,
        previous: &CsrMatrix,
        design: &CsrMatrix,
        time: &[f64],
        columns: Vec<usize>,
        selected: &[usize],
        biactive_closed_rows: Vec<usize>,
        biactive_open_rows: Vec<usize>,
        policy: ContactTangentPolicy,
    ) -> CaeResult<Self> {
        if !policy.rank_relative_tolerance.is_finite()
            || policy.rank_relative_tolerance < 32. * f64::EPSILON
            || policy.rank_relative_tolerance >= 1.
            || !policy.compatibility_relative_tolerance.is_finite()
            || policy.compatibility_relative_tolerance < 32. * f64::EPSILON
            || policy.compatibility_relative_tolerance >= 1.
            || current.nrows() != previous.nrows()
            || current.nrows() != design.nrows()
            || current.nrows() != time.len()
            || columns.iter().any(|&i| i >= current.ncols())
            || columns.iter().copied().collect::<BTreeSet<_>>().len() != columns.len()
            || selected.iter().any(|&i| i >= current.nrows())
            || selected.iter().copied().collect::<BTreeSet<_>>().len() != selected.len()
        {
            return Err(fail(
                "invalid selected contact constraint shape or rank policy",
            ));
        }
        let lookup: BTreeMap<_, _> = columns.iter().enumerate().map(|(i, &j)| (j, i)).collect();
        let mut candidates = Vec::new();
        for &i in selected {
            let mut row = Row {
                current: BTreeMap::new(),
                affine: BTreeMap::new(),
                source: i,
                affine_scale: 1.,
            };
            let (ci, cv) = current.row(i);
            for (&j, &v) in ci.iter().zip(cv) {
                if !v.is_finite() {
                    return Err(fail("nonfinite contact constraint row"));
                }
                if let Some(&k) = lookup.get(&j) {
                    *row.current.entry(k).or_default() += v;
                } else if v != 0. {
                    return Err(fail(
                        "active contact constraint depends on an excluded current coordinate",
                    ));
                }
            }
            for (matrix, offset) in [(previous, 0), (design, previous.ncols())] {
                let (ci, cv) = matrix.row(i);
                for (&j, &v) in ci.iter().zip(cv) {
                    *row.affine.entry(offset + j).or_default() += v;
                }
            }
            row.affine
                .insert(previous.ncols() + design.ncols(), time[i]);
            row.affine.retain(|_, v| *v != 0.);
            if !finite(&row) {
                return Err(fail("nonfinite contact affine constraint row"));
            }
            let scale = norm(&row.current);
            if !scale.is_finite() || scale == 0. {
                return Err(fail(
                    "active contact has no nonzero kinematic constraint row",
                ));
            }
            for v in row.current.values_mut().chain(row.affine.values_mut()) {
                *v /= scale;
            }
            row.affine_scale = norm(&row.affine).max(1.);
            if !finite(&row) || !row.affine_scale.is_finite() {
                return Err(fail("contact row normalization overflow"));
            }
            candidates.push(row);
        }
        let mut rows: Vec<Row> = Vec::new();
        let mut independent = vec![];
        let mut redundant = vec![];
        let (mut current_defect, mut affine_defect): (f64, f64) = (0., 0.);
        while !candidates.is_empty() {
            for candidate in &mut candidates {
                for _ in 0..2 {
                    for basis in &rows {
                        let alpha = dot(&candidate.current, &basis.current);
                        subtract(&mut candidate.current, &basis.current, alpha);
                        subtract(&mut candidate.affine, &basis.affine, alpha);
                    }
                }
                if !finite(candidate) {
                    return Err(fail("contact orthogonalization overflow"));
                }
            }
            let (pivot, pivot_norm) = candidates
                .iter()
                .enumerate()
                .map(|(i, r)| (i, norm(&r.current)))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap();
            if !pivot_norm.is_finite() {
                return Err(fail("nonfinite contact rank residual"));
            }
            if pivot_norm <= policy.rank_relative_tolerance {
                for row in candidates {
                    let a = norm(&row.affine);
                    let c = norm(&row.current);
                    if !a.is_finite()
                        || a > policy.compatibility_relative_tolerance * row.affine_scale
                    {
                        return Err(fail(
                            "redundant contact rows have incompatible boundary derivatives",
                        ));
                    }
                    current_defect = current_defect.max(c);
                    affine_defect = affine_defect.max(a);
                    redundant.push(row.source);
                }
                break;
            }
            let mut basis = candidates.remove(pivot);
            for v in basis.current.values_mut().chain(basis.affine.values_mut()) {
                *v /= pivot_norm;
            }
            if !finite(&basis) {
                return Err(fail("contact rank normalization overflow"));
            }
            independent.push(basis.source);
            rows.push(basis);
        }
        Ok(Self {
            columns,
            current_size: current.ncols(),
            previous_size: previous.ncols(),
            design_size: design.ncols(),
            rows,
            biactive_closed_rows,
            biactive_open_rows,
            independent_rows: independent,
            redundant_rows: redundant,
            maximum_redundant_current_norm: current_defect,
            maximum_redundant_affine_norm: affine_defect,
        })
    }
    pub fn rank(&self) -> usize {
        self.rows.len()
    }
    pub fn apply(
        &self,
        current: &[f64],
        previous: &[f64],
        design: &[f64],
        time: f64,
    ) -> CaeResult<Vec<f64>> {
        if current.len() != self.current_size
            || previous.len() != self.previous_size
            || design.len() != self.design_size
            || current
                .iter()
                .chain(previous)
                .chain(design)
                .any(|v| !v.is_finite())
            || !time.is_finite()
        {
            return Err(fail("invalid contact tangent direction"));
        }
        let mut out = current.to_vec();
        for row in &self.rows {
            let projection: f64 = row
                .current
                .iter()
                .map(|(&i, &v)| v * current[self.columns[i]])
                .sum();
            let affine: f64 = row
                .affine
                .iter()
                .map(|(&i, &v)| {
                    v * if i < self.previous_size {
                        previous[i]
                    } else if i < self.previous_size + self.design_size {
                        design[i - self.previous_size]
                    } else {
                        time
                    }
                })
                .sum();
            for (&i, &v) in &row.current {
                out[self.columns[i]] -= v * (projection + affine);
            }
        }
        if out.iter().any(|v| !v.is_finite()) {
            return Err(fail("contact tangent application overflow"));
        }
        Ok(out)
    }
    pub fn certify_constraint_rows(
        &self,
        current: &CsrMatrix,
        previous: &CsrMatrix,
        design: &CsrMatrix,
        time: &[f64],
        tolerance: f64,
    ) -> CaeResult<()> {
        if current.ncols() != self.current_size
            || previous.ncols() != self.previous_size
            || design.ncols() != self.design_size
            || current.nrows() != previous.nrows()
            || current.nrows() != design.nrows()
            || current.nrows() != time.len()
            || !tolerance.is_finite()
            || tolerance < 32. * f64::EPSILON
            || tolerance >= 1.
        {
            return Err(fail("invalid closed contact row certificate"));
        }
        let lookup: BTreeMap<_, _> = self
            .columns
            .iter()
            .enumerate()
            .map(|(i, &j)| (j, i))
            .collect();
        for row in 0..current.nrows() {
            let mut c = BTreeMap::new();
            let (cols, vals) = current.row(row);
            for (&j, &v) in cols.iter().zip(vals) {
                if v == 0. {
                    continue;
                }
                let &k = lookup
                    .get(&j)
                    .ok_or_else(|| fail("closed contact corner depends on an excluded state"))?;
                *c.entry(k).or_default() += v;
            }
            let mut a = BTreeMap::new();
            for (matrix, offset) in [(previous, 0), (design, self.previous_size)] {
                let (cols, vals) = matrix.row(row);
                for (&j, &v) in cols.iter().zip(vals) {
                    *a.entry(j + offset).or_default() += v;
                }
            }
            a.insert(self.previous_size + self.design_size, time[row]);
            c.retain(|_, v| *v != 0.);
            a.retain(|_, v| *v != 0.);
            if c.values().chain(a.values()).any(|v| !v.is_finite()) {
                return Err(fail("nonfinite closed contact corner row"));
            }
            let cscale = norm(&c);
            let ascale = norm(&a).max(cscale);
            let original = c.clone();
            for basis in &self.rows {
                let coefficient = dot(&original, &basis.current);
                subtract(&mut c, &basis.current, coefficient);
                subtract(&mut a, &basis.affine, coefficient);
            }
            if norm(&c) > tolerance * cscale.max(f64::MIN_POSITIVE)
                || norm(&a) > tolerance * ascale.max(f64::MIN_POSITIVE)
            {
                return Err(fail(
                    "closed contact corner is outside the active constraint span",
                ));
            }
        }
        Ok(())
    }
    pub fn project(&self, current: &[f64]) -> CaeResult<Vec<f64>> {
        self.apply(
            current,
            &vec![0.; self.previous_size],
            &vec![0.; self.design_size],
            0.,
        )
    }
    pub fn lift(&self, previous: &[f64], design: &[f64], time: f64) -> CaeResult<Vec<f64>> {
        self.apply(&vec![0.; self.current_size], previous, design, time)
    }
    pub fn transpose(&self, bar: &[f64]) -> CaeResult<ContactTangentBars> {
        if bar.len() != self.current_size || bar.iter().any(|v| !v.is_finite()) {
            return Err(fail("invalid contact tangent cotangent"));
        }
        let mut out = ContactTangentBars {
            current: bar.to_vec(),
            previous: vec![0.; self.previous_size],
            design: vec![0.; self.design_size],
            time: 0.,
        };
        for row in &self.rows {
            let projection: f64 = row
                .current
                .iter()
                .map(|(&i, &v)| v * bar[self.columns[i]])
                .sum();
            for (&i, &v) in &row.current {
                out.current[self.columns[i]] -= v * projection;
            }
            for (&i, &v) in &row.affine {
                if i < self.previous_size {
                    out.previous[i] -= v * projection;
                } else if i < self.previous_size + self.design_size {
                    out.design[i - self.previous_size] -= v * projection;
                } else {
                    out.time -= v * projection;
                }
            }
        }
        if out
            .current
            .iter()
            .chain(&out.previous)
            .chain(&out.design)
            .chain([&out.time])
            .any(|v| !v.is_finite())
        {
            return Err(fail("contact tangent transpose overflow"));
        }
        Ok(out)
    }
}
pub struct ContactTraceExtension {
    start: ContactConstraintTangent,
    end: ContactConstraintTangent,
}
fn through_trace(matrix: &CsrMatrix, trace: &CsrMatrix) -> CaeResult<CsrMatrix> {
    if matrix.ncols() != trace.ncols() {
        return Err(fail("native constraint trace shape"));
    }
    let mut observed = BTreeMap::new();
    for i in 0..trace.nrows() {
        let (columns, values) = trace.row(i);
        let nonzero: Vec<_> = columns
            .iter()
            .zip(values)
            .filter(|(_, v)| **v != 0.)
            .collect();
        if nonzero.len() != 1 || !nonzero[0].1.is_finite() {
            return Err(fail("native contact trace is not a nodal observation"));
        }
        let (&column, &weight) = nonzero[0];
        observed.entry(column).or_insert((i, weight));
    }
    let mut rows = vec![];
    let mut cols = vec![];
    let mut vals = vec![];
    for i in 0..matrix.nrows() {
        let (columns, values) = matrix.row(i);
        for (&column, &value) in columns.iter().zip(values) {
            if value == 0. {
                continue;
            }
            let &(row, scale) = observed
                .get(&column)
                .ok_or_else(|| fail("native contact constraint depends on an unobserved state"))?;
            let v = value / scale;
            if !v.is_finite() {
                return Err(fail("native contact trace conversion overflow"));
            }
            rows.push(i);
            cols.push(row);
            vals.push(v);
        }
    }
    CsrMatrix::from_triplets(matrix.nrows(), trace.nrows(), &rows, &cols, &vals)
        .map_err(|e| CaeError::contract(e.to_string()))
}
impl ContactTraceExtension {
    pub fn new(
        trace: &CsrMatrix,
        data: crate::moving_contact::contact_field::ContactTraceConstraintPair,
        policy: ContactTangentPolicy,
    ) -> CaeResult<Self> {
        let make = |data: crate::moving_contact::contact_field::ContactTraceConstraintData| -> CaeResult<_> {
            let c = data.contribution;
            let current = through_trace(&c.constraint_current.to_csr()?, trace)?;
            let previous = through_trace(&c.constraint_previous.to_csr()?, trace)?;
            let extension=ContactConstraintTangent::from_rows_with_origins(
                &current, &previous, &c.constraint_design.to_csr()?, &c.constraint_time,
                (0..trace.nrows()).collect(), &data.branches, &data.origins, policy,
            )?;
            if let Some(rows)=data.closed_rows {
                extension.certify_constraint_rows(&through_trace(&rows.current.to_csr()?,trace)?,
                    &through_trace(&rows.previous.to_csr()?,trace)?, &rows.design.to_csr()?, &rows.time,
                    policy.compatibility_relative_tolerance)?;
            }
            Ok(extension)
        };
        let start = make(data.start)?;
        if start
            .rows
            .iter()
            .any(|row| row.affine.keys().any(|&i| i < start.previous_size))
        {
            return Err(fail(
                "initial contact trace extension needs an earlier observed state",
            ));
        }
        let end = make(data.end)?;
        if start.design_size != end.design_size || start.current_size != end.current_size {
            return Err(fail("contact endpoint trace extension shape"));
        }
        Ok(Self { start, end })
    }
    pub(crate) fn from_selected_constraints(
        trace: &CsrMatrix,
        data: crate::moving_contact::contact_field::ContactTraceConstraintPair,
        start_rows: &[usize],
        end_rows: &[usize],
        policy: ContactTangentPolicy,
    ) -> CaeResult<Self> {
        let make = |data: crate::moving_contact::contact_field::ContactTraceConstraintData,
                    selected: &[usize]|
         -> CaeResult<_> {
            let c = data.contribution;
            let extension = ContactConstraintTangent::from_selected_rows(
                &through_trace(&c.constraint_current.to_csr()?, trace)?,
                &through_trace(&c.constraint_previous.to_csr()?, trace)?,
                &c.constraint_design.to_csr()?,
                &c.constraint_time,
                (0..trace.nrows()).collect(),
                selected,
                policy,
            )?;
            if let Some(rows) = data.closed_rows {
                extension.certify_constraint_rows(
                    &through_trace(&rows.current.to_csr()?, trace)?,
                    &through_trace(&rows.previous.to_csr()?, trace)?,
                    &rows.design.to_csr()?,
                    &rows.time,
                    policy.compatibility_relative_tolerance,
                )?;
            }
            Ok(extension)
        };
        let start = make(data.start, start_rows)?;
        if start
            .rows
            .iter()
            .any(|row| row.affine.keys().any(|&i| i < start.previous_size))
        {
            return Err(fail(
                "initial selected contact extension needs an earlier observed state",
            ));
        }
        let end = make(data.end, end_rows)?;
        if start.design_size != end.design_size || start.current_size != end.current_size {
            return Err(fail("selected contact endpoint extension shape"));
        }
        Ok(Self { start, end })
    }
}
impl implexity_solve::multirate_coupling::TraceDerivativeExtension for ContactTraceExtension {
    fn tangent(
        &self,
        start: &[f64],
        end: &[f64],
        design: Option<&[f64]>,
        time: f64,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        let zero_design = vec![0.; self.start.design_size];
        let x = design.unwrap_or(&zero_design);
        let a = self
            .start
            .apply(start, &vec![0.; self.start.previous_size], x, time)?;
        let b = self.end.apply(end, &a, x, time)?;
        Ok((a, b))
    }
    fn adjoint(
        &self,
        start: &[f64],
        end: &[f64],
    ) -> CaeResult<implexity_solve::multirate_coupling::TraceExtensionCotangent> {
        if start.len() != self.start.current_size {
            return Err(fail("contact start trace cotangent shape"));
        }
        let b = self.end.transpose(end)?;
        let combined: Vec<_> = start.iter().zip(&b.previous).map(|(a, b)| a + b).collect();
        let a = self.start.transpose(&combined)?;
        if a.previous.iter().any(|v| *v != 0.) {
            return Err(fail("contact start trace cotangent needs earlier state"));
        }
        let design = a.design.iter().zip(&b.design).map(|(a, b)| a + b).collect();
        Ok(
            implexity_solve::multirate_coupling::TraceExtensionCotangent {
                trace_start: a.current,
                trace_end: b.current,
                design,
                time_scale: a.time + b.time,
            },
        )
    }
}
