// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use faer::{Mat, MatRef, Side, c64};

use crate::error::LinalgError;

#[derive(Clone, Debug, PartialEq)]
pub struct DenseMatrix {
    pub nrows: usize,
    pub ncols: usize,
    pub data: Vec<f64>,
}

impl DenseMatrix {


    pub fn new(nrows: usize, ncols: usize, data: Vec<f64>) -> Result<Self, LinalgError> {
        if data.len() == nrows * ncols {
            Ok(Self { nrows, ncols, data })
        } else {
            Err(LinalgError::Shape(format!("{} entries for {nrows}×{ncols}", data.len())))
        }
    }

    #[must_use]
    pub fn zeros(nrows: usize, ncols: usize) -> Self {
        Self { nrows, ncols, data: vec![0.0; nrows * ncols] }
    }

    #[must_use]
    pub fn identity(n: usize) -> Self {
        let mut m = Self::zeros(n, n);
        for i in 0..n {
            m.data[i * n + i] = 1.0;
        }
        m
    }



    #[must_use]
    pub fn get(&self, i: usize, j: usize) -> f64 {
        self.data[i * self.ncols + j]
    }

    #[must_use]
    pub fn transpose(&self) -> Self {
        let mut t = Self::zeros(self.ncols, self.nrows);
        for i in 0..self.nrows {
            for j in 0..self.ncols {
                t.data[j * self.nrows + i] = self.data[i * self.ncols + j];
            }
        }
        t
    }



    pub fn matmul(&self, other: &Self) -> Result<Self, LinalgError> {
        if self.ncols != other.nrows {
            return Err(LinalgError::Shape("matmul: inner dimensions differ".into()));
        }
        let c = self.to_faer() * other.to_faer();
        Ok(Self::from_faer(c.as_ref()))
    }



    pub fn matvec(&self, x: &[f64]) -> Result<Vec<f64>, LinalgError> {
        if x.len() != self.ncols {
            return Err(LinalgError::Shape("matvec: dimension mismatch".into()));
        }
        Ok((0..self.nrows)
            .map(|i| self.data[i * self.ncols..(i + 1) * self.ncols].iter().zip(x).map(|(a, b)| a * b).sum())
            .collect())
    }

    #[must_use]
    pub fn to_faer(&self) -> Mat<f64> {
        Mat::from_fn(self.nrows, self.ncols, |i, j| self.data[i * self.ncols + j])
    }

    #[must_use]
    pub fn from_faer(m: MatRef<'_, f64>) -> Self {
        let (r, c) = (m.nrows(), m.ncols());
        let mut data = Vec::with_capacity(r * c);
        for i in 0..r {
            for j in 0..c {
                data.push(m[(i, j)]);
            }
        }
        Self { nrows: r, ncols: c, data }
    }

    fn require_finite(&self, what: &str) -> Result<(), LinalgError> {
        if self.data.iter().all(|v| v.is_finite()) {
            Ok(())
        } else {
            Err(LinalgError::NonFinite(format!("{what}: array must not contain infs or NaNs")))
        }
    }

    fn require_square(&self, what: &str) -> Result<(), LinalgError> {
        if self.nrows == self.ncols {
            Ok(())
        } else {
            Err(LinalgError::Shape(format!(
                "{what}: matrix must be square, got {}×{}",
                self.nrows, self.ncols
            )))
        }
    }



    pub fn norm2(&self) -> Result<f64, LinalgError> {
        Ok(singular_values(self)?.first().copied().unwrap_or(0.0))
    }

    #[must_use]
    pub fn norm_fro(&self) -> f64 {
        self.data.iter().map(|v| v * v).sum::<f64>().sqrt()
    }
}

#[derive(Clone, Debug)]
pub struct DenseLu {
    lu: faer::linalg::solvers::PartialPivLu<f64>,
    n: usize,
}

impl DenseLu {


    pub fn new(a: &DenseMatrix) -> Result<Self, LinalgError> {
        a.require_square("lu_factor")?;
        a.require_finite("lu_factor")?;
        let lu = a.to_faer().partial_piv_lu();
        let u = lu.U();
        if let Some(k) = (0..a.nrows).find(|&k| u[(k, k)] == 0.0) {
            return Err(LinalgError::Singular(format!("exactly zero pivot U[{k},{k}]")));
        }
        Ok(Self { lu, n: a.nrows })
    }



    pub fn solve(&self, b: &[f64], m: usize, transpose: bool) -> Result<Vec<f64>, LinalgError> {
        use faer::linalg::solvers::Solve;
        if b.len() != self.n * m {
            return Err(LinalgError::Shape(format!(
                "lu_solve: right-hand side of length {} for {}×{m}",
                b.len(),
                self.n
            )));
        }
        let mut rhs = Mat::from_fn(self.n, m, |i, j| b[i * m + j]);
        if transpose {
            self.lu.solve_transpose_in_place(rhs.as_mut());
        } else {
            self.lu.solve_in_place(rhs.as_mut());
        }
        Ok(DenseMatrix::from_faer(rhs.as_ref()).data)
    }
}



pub fn solve(a: &DenseMatrix, b: &[f64], m: usize) -> Result<Vec<f64>, LinalgError> {
    DenseLu::new(a)?.solve(b, m, false)
}



pub fn inv(a: &DenseMatrix) -> Result<DenseMatrix, LinalgError> {
    let n = a.nrows;
    let x = DenseLu::new(a)?.solve(&DenseMatrix::identity(n).data, n, false)?;
    DenseMatrix::new(n, n, x)
}



pub fn det(a: &DenseMatrix) -> Result<f64, LinalgError> {
    a.require_square("det")?;
    Ok(a.to_faer().determinant())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Svd {
    pub u: DenseMatrix,
    pub s: Vec<f64>,
    pub vt: DenseMatrix,
}



pub fn svd(a: &DenseMatrix) -> Result<Svd, LinalgError> {
    a.require_finite("svd")?;
    let s = a
        .to_faer()
        .thin_svd()
        .map_err(|e| LinalgError::NoConvergence(format!("SVD did not converge: {e:?}")))?;
    let k = a.nrows.min(a.ncols);
    let sv = s.S().column_vector();
    let vals: Vec<f64> = (0..k).map(|i| sv[i]).collect();
    Ok(Svd { u: DenseMatrix::from_faer(s.U()), s: vals, vt: DenseMatrix::from_faer(s.V().transpose()) })
}



pub fn svd_full(a: &DenseMatrix) -> Result<Svd, LinalgError> {
    a.require_finite("svd")?;
    let s =
        a.to_faer().svd().map_err(|e| LinalgError::NoConvergence(format!("SVD did not converge: {e:?}")))?;
    let k = a.nrows.min(a.ncols);
    let sv = s.S().column_vector();
    let vals: Vec<f64> = (0..k).map(|i| sv[i]).collect();
    Ok(Svd { u: DenseMatrix::from_faer(s.U()), s: vals, vt: DenseMatrix::from_faer(s.V().transpose()) })
}



pub fn singular_values(a: &DenseMatrix) -> Result<Vec<f64>, LinalgError> {
    a.require_finite("svd")?;
    if a.nrows == 0 || a.ncols == 0 {
        return Ok(Vec::new());
    }
    let mut s = a.to_faer().singular_values().map_err(|e| LinalgError::NoConvergence(format!("{e:?}")))?;
    s.sort_by(|x, y| y.total_cmp(x));
    Ok(s)
}



pub fn matrix_rank(a: &DenseMatrix, tol: Option<f64>) -> Result<usize, LinalgError> {
    let s = singular_values(a)?;
    let smax = s.first().copied().unwrap_or(0.0);
    let tol = tol.unwrap_or(smax * a.nrows.max(a.ncols) as f64 * f64::EPSILON);
    Ok(s.iter().filter(|&&v| v > tol).count())
}



pub fn cond(a: &DenseMatrix) -> Result<f64, LinalgError> {
    let s = singular_values(a)?;
    match (s.first(), s.last()) {
        (Some(&hi), Some(&lo)) => Ok(if lo == 0.0 { f64::INFINITY } else { hi / lo }),
        _ => Err(LinalgError::Shape("cond of an empty matrix".into())),
    }
}




pub fn lstsq(a: &DenseMatrix, b: &[f64], rcond: Option<f64>) -> Result<Vec<f64>, LinalgError> {
    if b.len() != a.nrows {
        return Err(LinalgError::Shape(format!(
            "lstsq: right-hand side of length {} for {} rows",
            b.len(),
            a.nrows
        )));
    }
    match lstsq_svd(a, b, rcond) {
        Err(LinalgError::NoConvergence(first)) => {
            let largest = a.data.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
            if largest > 0.0 && largest.is_finite() {

                #[allow(clippy::cast_possible_truncation)]
                let scale = 2.0_f64.powi(-largest.log2().round() as i32);
                let scaled = DenseMatrix {
                    nrows: a.nrows,
                    ncols: a.ncols,
                    data: a.data.iter().map(|v| v * scale).collect(),
                };
                let rhs: Vec<f64> = b.iter().map(|v| v * scale).collect();
                if let Ok(x) = lstsq_svd(&scaled, &rhs, rcond) {
                    return Ok(x);
                }
            }
            lstsq_qr(a, b, rcond).map_err(|e| {
                LinalgError::NoConvergence(format!("{first}; column-pivoted QR fallback failed: {e}"))
            })
        }
        other => other,
    }
}

fn lstsq_svd(a: &DenseMatrix, b: &[f64], rcond: Option<f64>) -> Result<Vec<f64>, LinalgError> {
    let d = svd(a)?;
    let k = d.s.len();
    let smax = d.s.first().copied().unwrap_or(0.0);
    let cut = rcond.unwrap_or(f64::EPSILON) * smax;
    let mut x = vec![0.0; a.ncols];
    for r in 0..k {
        if d.s[r] > cut {
            let coef: f64 = (0..a.nrows).map(|i| d.u.get(i, r) * b[i]).sum::<f64>() / d.s[r];
            for (j, xj) in x.iter_mut().enumerate() {
                *xj += coef * d.vt.get(r, j);
            }
        }
    }
    Ok(x)
}



pub fn lstsq_qr(a: &DenseMatrix, b: &[f64], rcond: Option<f64>) -> Result<Vec<f64>, LinalgError> {
    if b.len() != a.nrows {
        return Err(LinalgError::Shape(format!(
            "lstsq: right-hand side of length {} for {} rows",
            b.len(),
            a.nrows
        )));
    }
    let mut x = vec![0.0; a.ncols];
    if a.nrows == 0 || a.ncols == 0 {
        return Ok(x);
    }
    let (q, r, perm) = qr_col_piv(a)?;
    let k = q.ncols.min(r.nrows);
    #[allow(clippy::cast_precision_loss)]
    let cut = rcond.unwrap_or(f64::EPSILON * a.nrows.max(a.ncols) as f64) * r.get(0, 0).abs();
    let rank = (0..k).take_while(|&j| r.get(j, j).abs() > cut).count();
    let qtb: Vec<f64> = (0..rank).map(|j| (0..a.nrows).map(|i| q.get(i, j) * b[i]).sum()).collect();
    let mut y = vec![0.0; rank];
    for j in (0..rank).rev() {
        let mut acc = qtb[j];
        for (c, yc) in y.iter().enumerate().skip(j + 1) {
            acc -= r.get(j, c) * yc;
        }
        y[j] = acc / r.get(j, j);
    }
    for (j, yj) in y.into_iter().enumerate() {
        x[perm[j]] = yj;
    }
    if x.iter().any(|v| !v.is_finite()) {
        return Err(LinalgError::NonFinite("column-pivoted QR least-squares solution".into()));
    }
    Ok(x)
}



pub fn pinv(a: &DenseMatrix, rcond: Option<f64>) -> Result<DenseMatrix, LinalgError> {
    let d = svd(a)?;
    let smax = d.s.first().copied().unwrap_or(0.0);
    let cut = rcond.unwrap_or(1e-15) * smax;
    let mut p = DenseMatrix::zeros(a.ncols, a.nrows);
    for (r, &s) in d.s.iter().enumerate() {
        if s > cut {
            for i in 0..a.ncols {
                for j in 0..a.nrows {
                    p.data[i * a.nrows + j] += d.vt.get(r, i) * d.u.get(j, r) / s;
                }
            }
        }
    }
    Ok(p)
}



pub fn null_space(a: &DenseMatrix, rcond: Option<f64>) -> Result<DenseMatrix, LinalgError> {
    let d = svd_full(a)?;
    let smax = d.s.first().copied().unwrap_or(0.0);
    let tol = smax * rcond.unwrap_or(f64::EPSILON * a.nrows.max(a.ncols) as f64);
    let num = d.s.iter().filter(|&&s| s > tol).count();
    let n = a.ncols;
    let k = n - num;
    let mut z = DenseMatrix::zeros(n, k);
    for c in 0..k {
        for i in 0..n {
            z.data[i * k + c] = d.vt.get(num + c, i);
        }
    }
    Ok(z)
}



pub fn eigh(a: &DenseMatrix) -> Result<(Vec<f64>, DenseMatrix), LinalgError> {
    a.require_square("eigh")?;
    a.require_finite("eigh")?;
    let e = a
        .to_faer()
        .self_adjoint_eigen(Side::Lower)
        .map_err(|e| LinalgError::NoConvergence(format!("{e:?}")))?;
    let s = e.S().column_vector();
    let n = a.nrows;
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&i, &j| s[i].total_cmp(&s[j]));
    let vals = order.iter().map(|&i| s[i]).collect();
    let u = e.U();
    let mut v = DenseMatrix::zeros(n, n);
    for (c, &o) in order.iter().enumerate() {
        for i in 0..n {
            v.data[i * n + c] = u[(i, o)];
        }
    }
    Ok((vals, v))
}



pub fn eigvalsh(a: &DenseMatrix) -> Result<Vec<f64>, LinalgError> {
    Ok(eigh(a)?.0)
}



pub fn eigh_generalized(k: &DenseMatrix, m: &DenseMatrix) -> Result<(Vec<f64>, DenseMatrix), LinalgError> {
    k.require_square("eigh")?;
    m.require_square("eigh")?;
    if k.nrows != m.nrows {
        return Err(LinalgError::Shape("eigh: K and M differ in size".into()));
    }
    k.require_finite("eigh")?;
    m.require_finite("eigh")?;
    let n = k.nrows;
    let llt = m.to_faer().llt(Side::Lower).map_err(|e| {
        LinalgError::NotPositiveDefinite(format!("the leading minor of M is not positive definite: {e:?}"))
    })?;
    let l = llt.L().to_owned();
    let mut c = k.to_faer();
    faer::linalg::triangular_solve::solve_lower_triangular_in_place(l.as_ref(), c.as_mut(), faer::Par::Seq);
    let mut ct = c.transpose().to_owned();
    faer::linalg::triangular_solve::solve_lower_triangular_in_place(l.as_ref(), ct.as_mut(), faer::Par::Seq);
    let cs = DenseMatrix::from_faer(ct.as_ref());

    let mut sym = cs.clone();
    for i in 0..n {
        for j in 0..n {
            sym.data[i * n + j] = 0.5 * (cs.get(i, j) + cs.get(j, i));
        }
    }
    let (vals, y) = eigh(&sym)?;
    let mut v = y.to_faer();
    faer::linalg::triangular_solve::solve_upper_triangular_in_place(
        l.transpose(),
        v.as_mut(),
        faer::Par::Seq,
    );
    Ok((vals, DenseMatrix::from_faer(v.as_ref())))
}

#[derive(Clone, Debug)]
pub struct Eig {
    pub values: Vec<c64>,
    pub right: Vec<c64>,
    pub left: Vec<c64>,
    pub n: usize,
}

fn normalize_columns(v: &mut [c64], n: usize) {
    for c in 0..n {
        let norm = (0..n).map(|i| v[i * n + c].norm_sqr()).sum::<f64>().sqrt();
        if norm == 0.0 {
            continue;
        }
        let mut big = 0;
        for i in 0..n {
            if v[i * n + c].norm() > v[big * n + c].norm() {
                big = i;
            }
        }
        let phase = v[big * n + c].conj() / v[big * n + c].norm();
        for i in 0..n {
            v[i * n + c] = v[i * n + c] * phase / norm;
        }
    }
}




pub fn eig(a: &DenseMatrix) -> Result<Eig, LinalgError> {
    a.require_square("eig")?;
    a.require_finite("eig")?;
    let n = a.nrows;
    let e = faer::linalg::solvers::Eigen::new_from_real(a.to_faer().as_ref())
        .map_err(|e| LinalgError::NoConvergence(format!("eig did not converge: {e:?}")))?;
    let s = e.S().column_vector();
    let values: Vec<c64> = (0..n).map(|i| s[i]).collect();
    let u = e.U();
    let mut right: Vec<c64> = (0..n * n).map(|k| u[(k / n, k % n)]).collect();
    normalize_columns(&mut right, n);
    let vr = Mat::from_fn(n, n, |i, j| right[i * n + j]);
    let lu = vr.partial_piv_lu();
    let uu = lu.U();
    if (0..n).any(|k| uu[(k, k)].norm() == 0.0) {
        return Err(LinalgError::Singular("eigenvector matrix is singular (defective matrix)".into()));
    }
    let inv = {
        use faer::linalg::solvers::DenseSolveCore;
        lu.inverse()
    };
    let mut left: Vec<c64> = (0..n * n).map(|k| inv[(k % n, k / n)].conj()).collect();
    normalize_columns(&mut left, n);
    Ok(Eig { values, right, left, n })
}



pub fn solve_sylvester(
    a: &DenseMatrix,
    b: &DenseMatrix,
    q: &DenseMatrix,
) -> Result<DenseMatrix, LinalgError> {
    a.require_square("solve_sylvester")?;
    b.require_square("solve_sylvester")?;
    let (m, n) = (a.nrows, b.nrows);
    if q.nrows != m || q.ncols != n {
        return Err(LinalgError::Shape("solve_sylvester: Q must be m×n".into()));
    }
    let size = m * n;
    let mut k = DenseMatrix::zeros(size, size);
    for j in 0..n {
        for i in 0..m {
            let row = j * m + i;
            for l in 0..m {
                k.data[row * size + j * m + l] += a.get(i, l);
            }
            for l in 0..n {
                k.data[row * size + l * m + i] += b.get(l, j);
            }
        }
    }
    let rhs: Vec<f64> = (0..size).map(|r| q.get(r % m, r / m)).collect();
    let x = solve(&k, &rhs, 1)?;
    let mut out = DenseMatrix::zeros(m, n);
    for (r, v) in x.iter().enumerate() {
        out.data[(r % m) * n + r / m] = *v;
    }
    Ok(out)
}



pub fn qr_col_piv(c: &DenseMatrix) -> Result<(DenseMatrix, DenseMatrix, Vec<usize>), LinalgError> {
    c.require_finite("qr")?;
    let qr = c.to_faer().col_piv_qr();
    let q = DenseMatrix::from_faer(qr.compute_thin_Q().as_ref());
    let r = DenseMatrix::from_faer(qr.thin_R());
    let perm = qr.P().arrays().0.to_vec();
    Ok((q, r, perm))
}

