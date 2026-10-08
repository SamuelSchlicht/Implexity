// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use crate::error::LinalgError;
use crate::operator::LinearOperator;
use crate::sparse::CsrMatrix;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColumnOrdering {
    Natural,
    #[default]
    Colamd,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IluOptions {
    pub drop_tol: f64,
    pub fill_factor: f64,
    pub ordering: ColumnOrdering,
    pub diag_pivot_thresh: f64,
    pub equilibrate: bool,
}

impl Default for IluOptions {
    fn default() -> Self {
        Self {
            drop_tol: 1e-4,
            fill_factor: 10.0,
            ordering: ColumnOrdering::Colamd,
            diag_pivot_thresh: 0.1,
            equilibrate: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Ilu {
    n: usize,
    row_scale: Vec<f64>,
    col_scale: Vec<f64>,
    perm: Vec<usize>,
    l_ptr: Vec<usize>,
    l_idx: Vec<usize>,
    l_val: Vec<f64>,
    u_ptr: Vec<usize>,
    u_idx: Vec<usize>,
    u_val: Vec<f64>,
}

fn equilibrate(a: &CsrMatrix) -> Result<(Vec<f64>, Vec<f64>), LinalgError> {
    let (n, m) = a.shape();
    let smlnum = f64::MIN_POSITIVE;
    let bignum = 1.0 / smlnum;
    let mut r = vec![0.0f64; n];
    for (i, ri) in r.iter_mut().enumerate() {
        *ri = a.row(i).1.iter().fold(0.0f64, |acc, v| acc.max(v.abs()));
    }
    if let Some(i) = r.iter().position(|&v| v == 0.0) {
        return Err(LinalgError::Singular(format!("row {i} of the matrix is exactly zero")));
    }
    let amax = r.iter().copied().fold(0.0, f64::max);
    let rcmin = r.iter().copied().fold(f64::INFINITY, f64::min);
    let rcmax = amax;
    for v in &mut r {
        *v = 1.0 / v.max(smlnum).min(bignum);
    }
    let rowcnd = rcmin.max(smlnum) / rcmax.min(bignum);
    let mut c = vec![0.0f64; m];
    for (i, &ri) in r.iter().enumerate() {
        let (cols, vals) = a.row(i);
        for (&j, &v) in cols.iter().zip(vals) {
            c[j] = c[j].max(v.abs() * ri);
        }
    }
    if let Some(j) = c.iter().position(|&v| v == 0.0) {
        return Err(LinalgError::Singular(format!("column {j} of the matrix is exactly zero")));
    }
    let ccmin = c.iter().copied().fold(f64::INFINITY, f64::min);
    let ccmax = c.iter().copied().fold(0.0, f64::max);
    for v in &mut c {
        *v = 1.0 / v.max(smlnum).min(bignum);
    }
    let colcnd = ccmin.max(smlnum) / ccmax.min(bignum);

    let thresh = 0.1;
    let small = f64::MIN_POSITIVE / (f64::EPSILON * 0.5);
    let large = 1.0 / small;
    let scale_rows = !(rowcnd >= thresh && amax >= small && amax <= large);
    let scale_cols = colcnd < thresh;
    let r = if scale_rows { r } else { vec![1.0; n] };
    let c = if scale_cols { c } else { vec![1.0; m] };
    Ok((r, c))
}

fn colamd_order(a: &CsrMatrix) -> Result<Vec<usize>, LinalgError> {
    use faer::dyn_stack::{MemBuffer, MemStack};
    let csc = a.to_csc();
    let (m, n) = csc.shape();
    let mut perm = vec![0usize; n];
    let mut perm_inv = vec![0usize; n];
    let req = faer::sparse::linalg::colamd::order_scratch::<usize>(m, n, csc.nnz());
    let mut mem = MemBuffer::try_new(req).map_err(|_| LinalgError::OutOfMemory("COLAMD workspace".into()))?;
    faer::sparse::linalg::colamd::order(
        &mut perm,
        &mut perm_inv,
        csc.as_faer().symbolic(),
        faer::sparse::linalg::colamd::Control::default(),
        MemStack::new(&mut mem),
    )?;
    Ok(perm)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn count_from_f64(x: f64) -> usize {
    if x.is_nan() || x <= 0.0 { 0 } else { x.min(usize::MAX as f64) as usize }
}

fn keep_largest(entries: &mut Vec<(usize, f64)>, keep: usize) {
    if entries.len() > keep {
        entries.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()).then(a.0.cmp(&b.0)));
        entries.truncate(keep);
    }
    entries.sort_by_key(|e| e.0);
}

impl Ilu {


    #[allow(clippy::too_many_lines)]
    pub fn new(a: &CsrMatrix, opts: &IluOptions) -> Result<Self, LinalgError> {
        let (n, m) = a.shape();
        if n != m {
            return Err(LinalgError::Shape(format!("ILU of a non-square {n}×{m} matrix")));
        }
        let valid = opts.drop_tol.is_finite()
            && opts.drop_tol >= 0.0
            && opts.fill_factor.is_finite()
            && opts.fill_factor >= 1.0
            && (0.0..=1.0).contains(&opts.diag_pivot_thresh);
        if !valid {
            return Err(LinalgError::Invalid(
                "ILU options: drop_tol ≥ 0, fill_factor ≥ 1, diag_pivot_thresh ∈ [0, 1]".into(),
            ));
        }
        if !a.is_finite() {
            return Err(LinalgError::NonFinite("matrix entries must be finite".into()));
        }
        let (row_scale, col_scale) =
            if opts.equilibrate { equilibrate(a)? } else { (vec![1.0; n], vec![1.0; n]) };
        let mut perm: Vec<usize> = match opts.ordering {
            ColumnOrdering::Natural => (0..n).collect(),
            ColumnOrdering::Colamd => colamd_order(a)?,
        };
        let mut iperm = vec![0usize; n];
        for (p, &c) in perm.iter().enumerate() {
            iperm[c] = p;
        }

        let mut l_ptr = vec![0usize];
        let mut l_idx: Vec<usize> = Vec::new();
        let mut l_val: Vec<f64> = Vec::new();
        let mut u_ptr = vec![0usize];
        let mut u_col: Vec<usize> = Vec::new();
        let mut u_val: Vec<f64> = Vec::new();
        let mut u_diag = vec![0.0f64; n];
        let mut w = vec![0.0f64; n];
        let mut in_w: Vec<bool> = vec![false; n];
        let mut pattern: Vec<usize> = Vec::new();
        for i in 0..n {
            let (cols, vals) = a.row(i);
            let row_nnz = cols.len().max(1);
            let lfil = count_from_f64((opts.fill_factor * row_nnz as f64 / 2.0).ceil());
            pattern.clear();
            let mut tnorm = 0.0f64;
            for (&j, &v) in cols.iter().zip(vals) {
                let s = row_scale[i] * v * col_scale[j];
                let p = iperm[j];
                if !in_w[p] {
                    in_w[p] = true;
                    pattern.push(p);
                }
                w[p] += s;
                tnorm = tnorm.max(s.abs());
            }
            let tau = opts.drop_tol * tnorm;

            let mut pending: std::collections::BinaryHeap<std::cmp::Reverse<usize>> =
                pattern.iter().copied().filter(|&p| p < i).map(std::cmp::Reverse).collect();
            let mut l_row: Vec<(usize, f64)> = Vec::new();
            while let Some(std::cmp::Reverse(k)) = pending.pop() {
                if !in_w[k] {
                    continue;
                }
                let lk = w[k] / u_diag[k];
                w[k] = 0.0;
                in_w[k] = false;
                if lk.abs() < tau || lk == 0.0 {
                    continue;
                }
                l_row.push((k, lk));
                for t in u_ptr[k]..u_ptr[k + 1] {
                    let p = iperm[u_col[t]];
                    if !in_w[p] {
                        in_w[p] = true;
                        pattern.push(p);
                        w[p] = 0.0;
                        if p < i {
                            pending.push(std::cmp::Reverse(p));
                        }
                    }
                    w[p] -= lk * u_val[t];
                }
            }

            let mut u_row: Vec<(usize, f64)> = Vec::new();
            let mut diag = 0.0;
            for &p in &pattern {
                if in_w[p] && p >= i {
                    if p == i {
                        diag = w[p];
                    } else if w[p].abs() >= tau && w[p] != 0.0 {
                        u_row.push((p, w[p]));
                    }
                }
            }
            for &p in &pattern {
                w[p] = 0.0;
                in_w[p] = false;
            }
            keep_largest(&mut l_row, lfil);
            keep_largest(&mut u_row, lfil);

            if let Some((jmax, vmax)) =
                u_row.iter().copied().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()).then(b.0.cmp(&a.0)))
                && diag.abs() < opts.diag_pivot_thresh * vmax.abs()
            {
                let (ci, cj) = (perm[i], perm[jmax]);
                perm.swap(i, jmax);
                iperm[ci] = jmax;
                iperm[cj] = i;
                for e in &mut u_row {
                    if e.0 == jmax {
                        *e = (jmax, diag);
                    }
                }
                u_row.retain(|e| e.1 != 0.0);
                diag = vmax;
            }
            if diag == 0.0 {
                diag = (1e-4 + opts.drop_tol) * if tnorm > 0.0 { tnorm } else { 1.0 };
            }
            u_diag[i] = diag;
            for (k, v) in l_row {
                l_idx.push(k);
                l_val.push(v);
            }
            l_ptr.push(l_idx.len());
            for (p, v) in u_row {
                u_col.push(perm[p]);
                u_val.push(v);
            }
            u_ptr.push(u_col.len());
        }

        let mut up = vec![0usize];
        let mut ui = Vec::with_capacity(u_col.len() + n);
        let mut uv = Vec::with_capacity(u_col.len() + n);
        for i in 0..n {
            ui.push(i);
            uv.push(u_diag[i]);
            let mut row: Vec<(usize, f64)> =
                (u_ptr[i]..u_ptr[i + 1]).map(|t| (iperm[u_col[t]], u_val[t])).collect();
            row.sort_by_key(|e| e.0);
            for (p, v) in row {
                ui.push(p);
                uv.push(v);
            }
            up.push(ui.len());
        }
        Ok(Self { n, row_scale, col_scale, perm, l_ptr, l_idx, l_val, u_ptr: up, u_idx: ui, u_val: uv })
    }



    pub fn zero_fill(a: &CsrMatrix) -> Result<Self, LinalgError> {
        let (n, m) = a.shape();
        if n != m {
            return Err(LinalgError::Shape(format!("ILU(0) of a non-square {n}×{m} matrix")));
        }
        let ptr = a.indptr();
        let idx = a.indices();
        let mut val = a.data().to_vec();
        let mut diag_pos = vec![usize::MAX; n];
        for (i, dp) in diag_pos.iter_mut().enumerate() {
            if let Ok(off) = idx[ptr[i]..ptr[i + 1]].binary_search(&i) {
                *dp = ptr[i] + off;
            }
            if *dp == usize::MAX {
                return Err(LinalgError::Singular(format!("ILU(0): no diagonal entry in row {i}")));
            }
        }
        let mut where_ = vec![usize::MAX; n];
        for i in 0..n {
            for (k, &c) in idx.iter().enumerate().take(ptr[i + 1]).skip(ptr[i]) {
                where_[c] = k;
            }
            for k in ptr[i]..ptr[i + 1] {
                let c = idx[k];
                if c >= i {
                    break;
                }
                let piv = val[diag_pos[c]];
                if piv == 0.0 {
                    return Err(LinalgError::Singular(format!("ILU(0): zero pivot in row {c}")));
                }
                let lk = val[k] / piv;
                val[k] = lk;
                for t in diag_pos[c] + 1..ptr[c + 1] {
                    let j = idx[t];
                    let q = where_[j];
                    if q != usize::MAX {
                        val[q] -= lk * val[t];
                    }
                }
            }
            for &c in &idx[ptr[i]..ptr[i + 1]] {
                where_[c] = usize::MAX;
            }
            if val[diag_pos[i]] == 0.0 {
                return Err(LinalgError::Singular(format!("ILU(0): zero pivot in row {i}")));
            }
        }
        let mut l_ptr = vec![0];
        let mut l_idx: Vec<usize> = Vec::new();
        let mut l_val: Vec<f64> = Vec::new();
        let mut u_ptr = vec![0];
        let mut u_idx = Vec::new();
        let mut u_val: Vec<f64> = Vec::new();
        for i in 0..n {
            u_idx.push(i);
            u_val.push(val[diag_pos[i]]);
            for k in ptr[i]..ptr[i + 1] {
                match idx[k].cmp(&i) {
                    core::cmp::Ordering::Less => {
                        l_idx.push(idx[k]);
                        l_val.push(val[k]);
                    }
                    core::cmp::Ordering::Greater => {
                        u_idx.push(idx[k]);
                        u_val.push(val[k]);
                    }
                    core::cmp::Ordering::Equal => {}
                }
            }
            l_ptr.push(l_idx.len());
            u_ptr.push(u_idx.len());
        }
        Ok(Self {
            n,
            row_scale: vec![1.0; n],
            col_scale: vec![1.0; n],
            perm: (0..n).collect(),
            l_ptr,
            l_idx,
            l_val,
            u_ptr,
            u_idx,
            u_val,
        })
    }

    #[must_use]
    pub fn n(&self) -> usize {
        self.n
    }

    #[must_use]
    pub fn nnz(&self) -> usize {
        self.l_val.len() + self.u_val.len()
    }

    #[must_use]
    pub fn nbytes(&self) -> usize {
        self.nnz() * (size_of::<f64>() + size_of::<usize>())
            + (self.l_ptr.len() + self.u_ptr.len() + self.perm.len()) * size_of::<usize>()
            + (self.row_scale.len() + self.col_scale.len()) * size_of::<f64>()
    }



    pub fn solve(&self, b: &[f64], transpose: bool) -> Result<Vec<f64>, LinalgError> {
        let n = self.n;
        if b.len() != n {
            return Err(LinalgError::Shape(format!("ILU solve: vector of {} for n = {n}", b.len())));
        }
        if transpose {
            let mut v: Vec<f64> = self.perm.iter().map(|&c| b[c] * self.col_scale[c]).collect();
            for i in 0..n {
                v[i] /= self.u_val[self.u_ptr[i]];
                let vi = v[i];
                for t in self.u_ptr[i] + 1..self.u_ptr[i + 1] {
                    v[self.u_idx[t]] -= self.u_val[t] * vi;
                }
            }
            for i in (0..n).rev() {
                let vi = v[i];
                for t in self.l_ptr[i]..self.l_ptr[i + 1] {
                    v[self.l_idx[t]] -= self.l_val[t] * vi;
                }
            }
            Ok(v.iter().zip(&self.row_scale).map(|(y, r)| y * r).collect())
        } else {
            let mut z: Vec<f64> = b.iter().zip(&self.row_scale).map(|(v, r)| v * r).collect();
            for i in 0..n {
                let mut acc = z[i];
                for t in self.l_ptr[i]..self.l_ptr[i + 1] {
                    acc -= self.l_val[t] * z[self.l_idx[t]];
                }
                z[i] = acc;
            }
            for i in (0..n).rev() {
                let mut acc = z[i];
                for t in self.u_ptr[i] + 1..self.u_ptr[i + 1] {
                    acc -= self.u_val[t] * z[self.u_idx[t]];
                }
                z[i] = acc / self.u_val[self.u_ptr[i]];
            }
            let mut x = vec![0.0; n];
            for (pos, &col) in self.perm.iter().enumerate() {
                x[col] = z[pos] * self.col_scale[col];
            }
            Ok(x)
        }
    }
}

impl LinearOperator for Ilu {
    fn n(&self) -> usize {
        self.n
    }
    fn apply(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        let r = self.solve(x, false)?;
        if y.len() != r.len() {
            return Err(LinalgError::Shape("ILU apply: output length".into()));
        }
        y.copy_from_slice(&r);
        Ok(())
    }
}

