// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::f64::consts::PI;

use faer::Spec;
use faer::diag::Diag;
use faer::dyn_stack::{MemBuffer, MemStack};
use faer::linalg::evd::ComputeEigenvectors;
use faer::linalg::gevd::{gevd_cplx, gevd_scratch};
use faer::linalg::solvers::Solve;
use faer::{Mat, Par};
use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use serde_json::{Value, json};

pub use faer::c64;

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ComplexMatrix {
    pub nrows: usize,
    pub ncols: usize,
    pub data: Vec<c64>,
}

impl ComplexMatrix {
    #[must_use]
    pub fn zeros(nrows: usize, ncols: usize) -> Self {
        Self { nrows, ncols, data: vec![c64::new(0.0, 0.0); nrows * ncols] }
    }
    #[must_use]
    pub fn identity(n: usize) -> Self {
        let mut m = Self::zeros(n, n);
        for i in 0..n {
            m.data[i * n + i] = c64::new(1.0, 0.0);
        }
        m
    }
    #[must_use]
    pub fn from_real(m: &DenseMatrix) -> Self {
        Self { nrows: m.nrows, ncols: m.ncols, data: m.data.iter().map(|&v| c64::new(v, 0.0)).collect() }
    }
    #[must_use]
    pub fn get(&self, i: usize, j: usize) -> c64 {
        self.data[i * self.ncols + j]
    }
    #[must_use]
    pub fn column(&self, j: usize) -> Vec<c64> {
        (0..self.nrows).map(|i| self.get(i, j)).collect()
    }
    fn to_faer(&self) -> Mat<c64> {
        Mat::from_fn(self.nrows, self.ncols, |i, j| self.get(i, j))
    }
    fn from_faer(m: faer::MatRef<'_, c64>) -> Self {
        let mut out = Self::zeros(m.nrows(), m.ncols());
        for i in 0..m.nrows() {
            for j in 0..m.ncols() {
                out.data[i * m.ncols() + j] = m[(i, j)];
            }
        }
        out
    }


    pub fn matmul(&self, other: &Self) -> CaeResult<Self> {
        if self.ncols != other.nrows {
            return Err(err("complex matmul: inner dimensions differ"));
        }
        let mut out = Self::zeros(self.nrows, other.ncols);
        for i in 0..self.nrows {
            for k in 0..self.ncols {
                let a = self.data[i * self.ncols + k];
                for j in 0..other.ncols {
                    out.data[i * other.ncols + j] += a * other.data[k * other.ncols + j];
                }
            }
        }
        Ok(out)
    }


    pub fn matvec(&self, x: &[c64]) -> CaeResult<Vec<c64>> {
        if x.len() != self.ncols {
            return Err(err("complex matvec: length mismatch"));
        }
        Ok((0..self.nrows)
            .map(|i| (0..self.ncols).map(|j| self.data[i * self.ncols + j] * x[j]).sum())
            .collect())
    }
    #[must_use]
    pub fn adjoint(&self) -> Self {
        let mut out = Self::zeros(self.ncols, self.nrows);
        for i in 0..self.nrows {
            for j in 0..self.ncols {
                out.data[j * self.nrows + i] = self.get(i, j).conj();
            }
        }
        out
    }
    #[must_use]
    pub fn norm_fro(&self) -> f64 {
        self.data.iter().map(c64::norm_sqr).sum::<f64>().sqrt()
    }


    pub fn singular_values(&self) -> CaeResult<Vec<f64>> {
        let s = self.to_faer().singular_values().map_err(|e| err(format!("complex SVD failed: {e:?}")))?;
        let mut s: Vec<f64> = s.into_iter().collect();
        s.sort_by(|a, b| b.total_cmp(a));
        Ok(s)
    }


    pub fn norm2(&self) -> CaeResult<f64> {
        Ok(self.singular_values()?.first().copied().unwrap_or(0.0))
    }


    pub fn cond(&self) -> CaeResult<f64> {
        let s = self.singular_values()?;
        match (s.first(), s.last()) {
            (Some(&hi), Some(&lo)) => Ok(if lo == 0.0 { f64::INFINITY } else { hi / lo }),
            _ => Err(err("cond of an empty matrix")),
        }
    }


    pub fn solve(&self, rhs: &Self) -> CaeResult<Self> {
        if self.nrows != self.ncols || rhs.nrows != self.nrows {
            return Err(err("complex solve: shape mismatch"));
        }
        let lu = self.to_faer().partial_piv_lu();
        let u = lu.U();
        if (0..self.nrows).any(|k| u[(k, k)].norm() == 0.0) {
            return Err(err("Singular matrix"));
        }
        let mut b = rhs.to_faer();
        lu.solve_in_place(b.as_mut());
        Ok(Self::from_faer(b.as_ref()))
    }


    pub fn rank(&self) -> CaeResult<usize> {
        let s = self.singular_values()?;
        let tol = s.first().copied().unwrap_or(0.0) * self.nrows.max(self.ncols) as f64 * f64::EPSILON;
        Ok(s.iter().filter(|&&v| v > tol).count())
    }
}

fn vdot(a: &[c64], b: &[c64]) -> c64 {
    a.iter().zip(b).map(|(x, y)| x.conj() * y).sum()
}

fn vnorm(a: &[c64]) -> f64 {
    a.iter().map(c64::norm_sqr).sum::<f64>().sqrt()
}

fn normalize_columns(m: &mut ComplexMatrix) {
    let n = m.nrows;
    for c in 0..m.ncols {
        let norm = (0..n).map(|i| m.get(i, c).norm_sqr()).sum::<f64>().sqrt();
        if norm == 0.0 {
            continue;
        }
        let big = (0..n).fold(0, |b, i| if m.get(i, c).norm() > m.get(b, c).norm() { i } else { b });
        let phase = m.get(big, c).conj() / m.get(big, c).norm();
        for i in 0..n {
            let v = m.data[i * m.ncols + c];
            m.data[i * m.ncols + c] = v * phase / norm;
        }
    }
}



pub fn eig_pairs(
    a: &ComplexMatrix,
    b: Option<&ComplexMatrix>,
) -> CaeResult<(Vec<c64>, ComplexMatrix, ComplexMatrix)> {
    let n = a.nrows;
    if a.ncols != n {
        return Err(err("A must be square"));
    }
    let b = b.cloned().unwrap_or_else(|| ComplexMatrix::identity(n));
    if (b.nrows, b.ncols) != (n, n) {
        return Err(err("B must match A"));
    }
    if a.data.iter().chain(&b.data).any(|v| !v.re.is_finite() || !v.im.is_finite()) {
        return Err(err("array must not contain infs or NaNs"));
    }
    if n <= 1 {

        let values = (0..n)
            .map(|_| {
                let (x, y) = (a.data[0], b.data[0]);
                if y.norm() == 0.0 { c64::new(f64::INFINITY, 0.0) } else { x / y }
            })
            .collect();
        let unit = ComplexMatrix::identity(n);
        return Ok((values, unit.clone(), unit));
    }
    let mut fa = a.to_faer();
    let mut fb = b.to_faer();
    let mut alpha = Diag::<c64>::zeros(n);
    let mut beta = Diag::<c64>::zeros(n);
    let mut ul = Mat::<c64>::zeros(n, n);
    let mut ur = Mat::<c64>::zeros(n, n);
    let par = Par::Seq;
    let mut mem = MemBuffer::new(gevd_scratch::<c64>(
        n,
        ComputeEigenvectors::Yes,
        ComputeEigenvectors::Yes,
        par,
        Spec::default(),
    ));
    gevd_cplx(
        fa.as_mut(),
        fb.as_mut(),
        alpha.as_mut(),
        beta.as_mut(),
        Some(ul.as_mut()),
        Some(ur.as_mut()),
        par,
        MemStack::new(&mut mem),
        Spec::default(),
    )
    .map_err(|e| err(format!("generalized eigenproblem did not converge: {e:?}")))?;
    let a_col = alpha.column_vector();
    let b_col = beta.column_vector();
    let values: Vec<c64> = (0..n)
        .map(|i| {
            let (x, y) = (a_col[i], b_col[i]);
            if y.norm() == 0.0 { c64::new(f64::INFINITY, 0.0) } else { x / y }
        })
        .collect();
    let mut left = ComplexMatrix::from_faer(ul.as_ref());
    let mut right = ComplexMatrix::from_faer(ur.as_ref());
    normalize_columns(&mut left);
    normalize_columns(&mut right);
    Ok((values, left, right))
}

fn reorder(values: &mut Vec<c64>, left: &mut ComplexMatrix, right: &mut ComplexMatrix, order: &[usize]) {
    let n = left.nrows;
    let pick = |m: &ComplexMatrix| {
        let mut out = ComplexMatrix::zeros(n, order.len());
        for (c, &j) in order.iter().enumerate() {
            for i in 0..n {
                out.data[i * order.len() + c] = m.get(i, j);
            }
        }
        out
    };
    *left = pick(left);
    *right = pick(right);
    *values = order.iter().map(|&i| values[i]).collect();
}

fn real_order(values: &[c64]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&i, &j| values[i].re.total_cmp(&values[j].re).then(values[i].im.total_cmp(&values[j].im)));
    order
}

#[derive(Clone, Debug, PartialEq)]
pub struct BiorthogonalEigenResult {
    pub eigenvalues: Vec<c64>,
    pub right_eigenvectors: ComplexMatrix,
    pub left_eigenvectors: ComplexMatrix,
    pub condition_numbers: Vec<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Real,
    Magnitude,
}



pub fn generalized_eig(
    a: &ComplexMatrix,
    b: Option<&ComplexMatrix>,
    n_modes: Option<usize>,
    sort_key: SortKey,
) -> CaeResult<BiorthogonalEigenResult> {
    let n = a.nrows;
    let bm = b.cloned().unwrap_or_else(|| ComplexMatrix::identity(n));
    let (mut vals, mut vl, mut vr) = eig_pairs(a, Some(&bm))?;
    let order = match sort_key {
        SortKey::Real => real_order(&vals),
        SortKey::Magnitude => {
            let mut o: Vec<usize> = (0..vals.len()).collect();
            o.sort_by(|&i, &j| vals[i].norm().total_cmp(&vals[j].norm()));
            o
        }
    };
    reorder(&mut vals, &mut vl, &mut vr, &order);
    let mut cond = Vec::with_capacity(n);
    for j in 0..vals.len() {
        let l = vl.column(j);
        let br = bm.matvec(&vr.column(j))?;
        let den = vdot(&l, &br).norm();
        cond.push(vnorm(&l) * vnorm(&br) / den.max(f64::MIN_POSITIVE));
    }
    if let Some(k) = n_modes {
        let keep: Vec<usize> = (0..k.min(vals.len())).collect();
        reorder(&mut vals, &mut vl, &mut vr, &keep);
        cond.truncate(k);
    }
    Ok(BiorthogonalEigenResult {
        eigenvalues: vals,
        right_eigenvectors: vr,
        left_eigenvectors: vl,
        condition_numbers: cond,
    })
}



pub fn eigenvalue_design_gradient(
    eigenvalue: c64,
    right: &[c64],
    left: &[c64],
    da: &[ComplexMatrix],
    db: Option<&[ComplexMatrix]>,
    b: Option<&ComplexMatrix>,
) -> CaeResult<Vec<c64>> {
    let n = right.len();
    if da.iter().any(|d| (d.nrows, d.ncols) != (n, n)) {
        return Err(err("dA trailing shape mismatch"));
    }
    if let Some(db) = db
        && (db.len() != da.len() || db.iter().any(|d| (d.nrows, d.ncols) != (n, n)))
    {
        return Err(err("dB must match dA"));
    }
    let bm = b.cloned().unwrap_or_else(|| ComplexMatrix::identity(n));
    let br = bm.matvec(right)?;
    let den = vdot(left, &br);
    if den.norm() <= 1e-14 * 1.0_f64.max(vnorm(left) * vnorm(&br)) {
        return Err(err("eigenpair is defective or too close to an exceptional point"));
    }
    da.iter()
        .enumerate()
        .map(|(k, d)| {
            let mut c = d.clone();
            if let Some(db) = db {
                for (x, y) in c.data.iter_mut().zip(&db[k].data) {
                    *x -= eigenvalue * y;
                }
            }
            Ok(vdot(left, &c.matvec(right)?) / den)
        })
        .collect()
}

#[must_use]
pub fn exceptional_point_diagnostics(
    result: &BiorthogonalEigenResult,
    condition_limit: f64,
    separation_tol: f64,
) -> Value {
    let vals = &result.eigenvalues;
    let mut sep = f64::INFINITY;
    for i in 0..vals.len() {
        for j in 0..vals.len() {
            if i != j {
                sep = sep.min((vals[i] - vals[j]).norm());
            }
        }
    }
    let max_cond = result.condition_numbers.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let max_cond = if result.condition_numbers.is_empty() { 0.0 } else { max_cond };
    json!({
        "max_eigen_condition": max_cond,
        "minimum_eigen_separation": if sep.is_finite() { json!(sep) } else { Value::Null },
        "near_exceptional_point": (!result.condition_numbers.is_empty() && max_cond > condition_limit) || sep < separation_tol,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpectralCluster {
    pub indices: Vec<usize>,
    pub eigenvalues: Vec<c64>,
    pub right: ComplexMatrix,
    pub left: ComplexMatrix,
    pub projector: ComplexMatrix,
    pub separation: f64,
    pub condition: f64,
}

fn columns(m: &ComplexMatrix, idx: &[usize]) -> ComplexMatrix {
    let mut out = ComplexMatrix::zeros(m.nrows, idx.len());
    for (c, &j) in idx.iter().enumerate() {
        for i in 0..m.nrows {
            out.data[i * idx.len() + c] = m.get(i, j);
        }
    }
    out
}

fn standard_spectrum(a: &ComplexMatrix) -> CaeResult<(Vec<c64>, ComplexMatrix, ComplexMatrix)> {
    let (mut vals, mut vl, mut vr) = eig_pairs(a, None)?;
    let order = real_order(&vals);
    reorder(&mut vals, &mut vl, &mut vr, &order);
    Ok((vals, vl, vr))
}



pub fn identify_cluster(a: &ComplexMatrix, seed_index: usize, radius: f64) -> CaeResult<SpectralCluster> {
    let (vals, vl, vr) = standard_spectrum(a)?;
    let seed = *vals.get(seed_index).ok_or_else(|| err("cluster seed index is out of range"))?;
    let mut idx: Vec<usize> = (0..vals.len()).filter(|&i| (vals[i] - seed).norm() <= radius).collect();
    if idx.is_empty() {
        idx.push(seed_index);
    }
    let l = columns(&vl, &idx);
    let r = columns(&vr, &idx);
    let g = l.adjoint().matmul(&r)?;
    if g.rank()? < g.nrows {
        return Err(err("cluster left/right bases are biorthogonally singular"));
    }
    let r = r.matmul(&g.solve(&ComplexMatrix::identity(g.nrows))?)?;
    let p = r.matmul(&l.adjoint())?;
    let outside: Vec<c64> = (0..vals.len()).filter(|i| !idx.contains(i)).map(|i| vals[i]).collect();
    let mut sep = f64::INFINITY;
    for &i in &idx {
        for o in &outside {
            sep = sep.min((vals[i] - o).norm());
        }
    }
    let condition = r.norm2()? * l.norm2()?;
    Ok(SpectralCluster {
        eigenvalues: idx.iter().map(|&i| vals[i]).collect(),
        indices: idx,
        right: r,
        left: l,
        projector: p,
        separation: sep,
        condition,
    })
}



pub fn cluster_mean_eigenvalue_gradient(
    cluster: &SpectralCluster,
    da: &[ComplexMatrix],
) -> CaeResult<Vec<c64>> {
    let n = cluster.projector.nrows;
    let m = cluster.indices.len() as f64;
    da.iter()
        .map(|d| {
            if (d.nrows, d.ncols) != (n, n) {
                return Err(err("dA trailing dimensions do not match cluster operator"));
            }
            let mut s = c64::new(0.0, 0.0);
            for a in 0..cluster.indices.len() {
                let l = cluster.left.column(a);
                let r = cluster.right.column(a);
                s += vdot(&l, &d.matvec(&r)?);
            }
            Ok(s / m)
        })
        .collect()
}



pub fn cluster_projector_directional_derivative(
    a: &ComplexMatrix,
    cluster: &SpectralCluster,
    da: &ComplexMatrix,
    separation_tol: f64,
) -> CaeResult<ComplexMatrix> {
    if cluster.separation <= separation_tol {
        return Err(err("cluster is not separated; exceptional-point/subspace relinearization required"));
    }
    let (vals, vl, vr) = standard_spectrum(a)?;
    let n = a.nrows;
    let mut pdot = ComplexMatrix::zeros(n, n);
    for &i in &cluster.indices {
        let mut li = vl.column(i);
        let ri = vr.column(i);
        let deni = vdot(&li, &ri);
        if deni.norm() < 1e-14 {
            return Err(err("defective eigenvector normalization"));
        }
        for v in &mut li {
            *v /= deni.conj();
        }
        for j in 0..n {
            if cluster.indices.contains(&j) {
                continue;
            }
            let mut lj = vl.column(j);
            let rj = vr.column(j);
            let denj = vdot(&lj, &rj);
            if denj.norm() < 1e-14 {
                return Err(err("defective complementary eigenvector normalization"));
            }
            for v in &mut lj {
                *v /= denj.conj();
            }
            let gap = vals[i] - vals[j];
            if gap.norm() <= separation_tol {
                return Err(err("cluster/complement gap collapsed"));
            }
            let coef_a = vdot(&lj, &da.matvec(&ri)?) / gap;
            let coef_b = vdot(&li, &da.matvec(&rj)?) / gap;
            for p in 0..n {
                for q in 0..n {
                    pdot.data[p * n + q] += coef_a * rj[p] * li[q].conj() + coef_b * ri[p] * lj[q].conj();
                }
            }
        }
    }
    Ok(pdot)
}

#[must_use]
pub fn exceptional_point_gate(cluster: &SpectralCluster, min_separation: f64, max_condition: f64) -> Value {
    let ok = cluster.separation > min_separation && cluster.condition < max_condition;
    json!({
        "admissible": ok,
        "cluster_separation": if cluster.separation.is_finite() { json!(cluster.separation) } else { Value::Null },
        "cluster_condition": cluster.condition,
        "reason": if ok { Value::Null } else { json!("exceptional-point/cluster conditioning requires subspace treatment or relinearization") },
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct RieszProjectorResult {
    pub projector: ComplexMatrix,
    pub rank: i64,
    pub idempotence_error: f64,
    pub contour_resolvent_condition: f64,
}

fn contour_point(center: c64, radius: f64, k: usize, quadrature: usize) -> (c64, c64) {
    let th = 2.0 * PI * (k as f64 + 0.5) / quadrature as f64;
    let e = c64::new(th.cos(), th.sin());
    (center + e * radius, e * radius / quadrature as f64)
}



pub fn riesz_projector(
    a: &ComplexMatrix,
    center: c64,
    radius: f64,
    quadrature: usize,
) -> CaeResult<RieszProjectorResult> {
    let n = a.nrows;
    if a.ncols != n || radius <= 0.0 || quadrature == 0 {
        return Err(err("invalid operator/contour"));
    }
    let id = ComplexMatrix::identity(n);
    let mut p = ComplexMatrix::zeros(n, n);
    let mut worst: f64 = 0.0;
    for k in 0..quadrature {
        let (z, w) = contour_point(center, radius, k, quadrature);
        let mut m = a.clone();
        for v in &mut m.data {
            *v = -*v;
        }
        for i in 0..n {
            m.data[i * n + i] += z;
        }
        worst = worst.max(m.cond()?);
        let r = m.solve(&id)?;
        for (x, y) in p.data.iter_mut().zip(&r.data) {
            *x += w * y;
        }
    }
    let trace: c64 = (0..n).map(|i| p.get(i, i)).sum();
    let pp = p.matmul(&p)?;
    let diff =
        ComplexMatrix { nrows: n, ncols: n, data: pp.data.iter().zip(&p.data).map(|(a, b)| a - b).collect() };
    #[allow(clippy::cast_possible_truncation)]
    let rank = trace.re.round() as i64;
    Ok(RieszProjectorResult {
        idempotence_error: diff.norm_fro() / p.norm_fro().max(1e-30),
        projector: p,
        rank,
        contour_resolvent_condition: worst,
    })
}



pub fn riesz_projector_derivative(
    a: &ComplexMatrix,
    da: &[ComplexMatrix],
    center: c64,
    radius: f64,
    quadrature: usize,
) -> CaeResult<Vec<ComplexMatrix>> {
    let n = a.nrows;
    if da.iter().any(|d| (d.nrows, d.ncols) != (n, n)) {
        return Err(err("dA dimension mismatch"));
    }
    let id = ComplexMatrix::identity(n);
    let mut out: Vec<ComplexMatrix> = da.iter().map(|_| ComplexMatrix::zeros(n, n)).collect();
    for k in 0..quadrature {
        let (z, w) = contour_point(center, radius, k, quadrature);
        let mut m = a.clone();
        for v in &mut m.data {
            *v = -*v;
        }
        for i in 0..n {
            m.data[i * n + i] += z;
        }
        let r = m.solve(&id)?;
        for (o, d) in out.iter_mut().zip(da) {
            let rdr = r.matmul(d)?.matmul(&r)?;
            for (x, y) in o.data.iter_mut().zip(&rdr.data) {
                *x += w * y;
            }
        }
    }
    Ok(out)
}



pub fn subspace_trace_response(
    a: &ComplexMatrix,
    observable: &ComplexMatrix,
    center: c64,
    radius: f64,
    quadrature: usize,
) -> CaeResult<(f64, RieszProjectorResult)> {
    let r = riesz_projector(a, center, radius, quadrature)?;
    let po = r.projector.matmul(observable)?;
    let trace: c64 = (0..po.nrows).map(|i| po.get(i, i)).sum();
    Ok((trace.re, r))
}



pub fn subspace_trace_gradient(
    a: &ComplexMatrix,
    da: &[ComplexMatrix],
    observable: &ComplexMatrix,
    center: c64,
    radius: f64,
    quadrature: usize,
) -> CaeResult<Vec<f64>> {
    let dp = riesz_projector_derivative(a, da, center, radius, quadrature)?;
    dp.iter()
        .map(|d| {
            let po = d.matmul(observable)?;
            Ok((0..po.nrows).map(|i| po.get(i, i)).sum::<c64>().re)
        })
        .collect()
}

