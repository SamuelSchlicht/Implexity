// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;

pub type Sp = CsrMatrix;

fn lin(e: impl std::fmt::Display) -> CaeError {
    CaeError::contract(e.to_string())
}


pub fn triplets(nrows: usize, ncols: usize, rows: &[usize], cols: &[usize], vals: &[f64]) -> CaeResult<Sp> {
    Sp::from_triplets(nrows, ncols, rows, cols, vals).map_err(lin)
}

#[must_use]
pub fn zeros(nrows: usize, ncols: usize) -> Sp {
    Sp::from_triplets(nrows, ncols, &[], &[], &[]).unwrap_or_else(|_| Sp::identity(0))
}

#[must_use]
pub fn eye(n: usize) -> Sp {
    Sp::identity(n)
}

#[must_use]
pub fn diag(d: &[f64]) -> Sp {
    Sp::diagonal_matrix(d)
}


pub fn mm(a: &Sp, b: &Sp) -> CaeResult<Sp> {
    a.matmul(b).map_err(lin)
}


pub fn mm3(a: &Sp, b: &Sp, c: &Sp) -> CaeResult<Sp> {
    mm(&mm(a, b)?, c)
}


pub fn add(a: &Sp, b: &Sp) -> CaeResult<Sp> {
    a.add_scaled(1.0, b, 1.0).map_err(lin)
}


pub fn sub(a: &Sp, b: &Sp) -> CaeResult<Sp> {
    a.add_scaled(1.0, b, -1.0).map_err(lin)
}

#[must_use]
pub fn scale(a: &Sp, alpha: f64) -> Sp {
    let data: Vec<f64> = a.data().iter().map(|v| v * alpha).collect();
    a.with_data(data).unwrap_or_else(|_| a.clone())
}

#[must_use]
pub fn neg(a: &Sp) -> Sp {
    scale(a, -1.0)
}

#[must_use]
pub fn t(a: &Sp) -> Sp {
    a.transpose()
}


pub fn mv(a: &Sp, x: &[f64]) -> CaeResult<Vec<f64>> {
    a.matvec(x).map_err(lin)
}

#[must_use]
pub fn entries(a: &Sp) -> Vec<(usize, usize, f64)> {
    let mut out = Vec::with_capacity(a.nnz());
    for i in 0..a.nrows() {
        let (c, v) = a.row(i);
        for (j, x) in c.iter().zip(v) {
            out.push((i, *j, *x));
        }
    }
    out
}


pub fn vstack(parts: &[&Sp]) -> CaeResult<Sp> {
    let ncols = parts.first().map_or(0, |p| p.ncols());
    let (mut r, mut c, mut v) = (Vec::new(), Vec::new(), Vec::new());
    let mut offset = 0;
    for p in parts {
        if p.ncols() != ncols {
            return Err(CaeError::contract("vstack: column counts differ"));
        }
        for (i, j, x) in entries(p) {
            r.push(offset + i);
            c.push(j);
            v.push(x);
        }
        offset += p.nrows();
    }
    triplets(offset, ncols, &r, &c, &v)
}


pub fn hstack(parts: &[&Sp]) -> CaeResult<Sp> {
    let nrows = parts.first().map_or(0, |p| p.nrows());
    let (mut r, mut c, mut v) = (Vec::new(), Vec::new(), Vec::new());
    let mut offset = 0;
    for p in parts {
        if p.nrows() != nrows {
            return Err(CaeError::contract("hstack: row counts differ"));
        }
        for (i, j, x) in entries(p) {
            r.push(i);
            c.push(offset + j);
            v.push(x);
        }
        offset += p.ncols();
    }
    triplets(nrows, offset, &r, &c, &v)
}


pub fn kron_eye(a: &Sp, k: usize) -> CaeResult<Sp> {
    let (mut r, mut c, mut v) = (Vec::new(), Vec::new(), Vec::new());
    for (i, j, x) in entries(a) {
        for d in 0..k {
            r.push(i * k + d);
            c.push(j * k + d);
            v.push(x);
        }
    }
    triplets(a.nrows() * k, a.ncols() * k, &r, &c, &v)
}


pub fn block_diag(blocks: &[Vec<f64>], rows: usize, cols: usize) -> CaeResult<Sp> {
    let (mut r, mut c, mut v) = (Vec::new(), Vec::new(), Vec::new());
    for (b, block) in blocks.iter().enumerate() {
        if block.len() != rows * cols {
            return Err(CaeError::contract("block_diag: block size mismatch"));
        }
        for i in 0..rows {
            for j in 0..cols {
                let x = block[i * cols + j];
                if x != 0.0 {
                    r.push(b * rows + i);
                    c.push(b * cols + j);
                    v.push(x);
                }
            }
        }
    }
    triplets(blocks.len() * rows, blocks.len() * cols, &r, &c, &v)
}


pub fn select_rows(a: &Sp, rows: &[usize]) -> CaeResult<Sp> {
    let (mut r, mut c, mut v) = (Vec::new(), Vec::new(), Vec::new());
    for (k, &i) in rows.iter().enumerate() {
        if i >= a.nrows() {
            return Err(CaeError::contract("row selection out of range"));
        }
        let (cols, vals) = a.row(i);
        for (j, x) in cols.iter().zip(vals) {
            r.push(k);
            c.push(*j);
            v.push(*x);
        }
    }
    triplets(rows.len(), a.ncols(), &r, &c, &v)
}


pub fn selector(cols: &[usize], ncols: usize) -> CaeResult<Sp> {
    let rows: Vec<usize> = (0..cols.len()).collect();
    triplets(cols.len(), ncols, &rows, cols, &vec![1.0; cols.len()])
}


pub fn sum(parts: &[Sp]) -> CaeResult<Sp> {
    let mut iter = parts.iter();
    let Some(first) = iter.next() else { return Err(CaeError::contract("empty sparse sum")) };
    let mut acc = first.clone();
    for p in iter {
        acc = add(&acc, p)?;
    }
    Ok(acc)
}


pub fn checked(a: Sp, shape: (usize, usize)) -> CaeResult<Sp> {
    if a.shape() != shape || !a.data().iter().all(|v| f64::is_finite(*v)) {
        return Err(CaeError::contract("finite shape-matched sparse chain required"));
    }
    Ok(a)
}

#[derive(Clone, Debug)]
pub struct SpMap {
    rows: Vec<usize>,
    cols: Vec<usize>,
    data: Vec<f64>,
    nrows: usize,
    ncols: usize,
}

impl SpMap {
    #[must_use]
    pub fn new(a: &Sp) -> Self {
        let e = entries(a);
        Self {
            rows: e.iter().map(|x| x.0).collect(),
            cols: e.iter().map(|x| x.1).collect(),
            data: e.iter().map(|x| x.2).collect(),
            nrows: a.nrows(),
            ncols: a.ncols(),
        }
    }

    #[must_use]
    pub fn nrows(&self) -> usize {
        self.nrows
    }

    #[must_use]
    pub fn ncols(&self) -> usize {
        self.ncols
    }

    #[must_use]
    pub fn apply<S: Scalar>(&self, x: &[S]) -> Vec<S> {
        let mut out = vec![S::zero(); self.nrows];
        for ((r, c), d) in self.rows.iter().zip(&self.cols).zip(&self.data) {
            out[*r] += x[*c] * *d;
        }
        out
    }

    #[must_use]
    pub fn apply_t<S: Scalar>(&self, x: &[S]) -> Vec<S> {
        let mut out = vec![S::zero(); self.ncols];
        for ((r, c), d) in self.rows.iter().zip(&self.cols).zip(&self.data) {
            out[*c] += x[*r] * *d;
        }
        out
    }

    #[must_use]
    pub fn apply_vec<S: Scalar, const K: usize>(&self, x: &[[S; K]]) -> Vec<[S; K]> {
        let mut out = vec![[S::zero(); K]; self.nrows];
        for ((r, c), d) in self.rows.iter().zip(&self.cols).zip(&self.data) {
            for a in 0..K {
                out[*r][a] += x[*c][a] * *d;
            }
        }
        out
    }

    #[must_use]
    pub fn apply_t_vec<S: Scalar, const K: usize>(&self, x: &[[S; K]]) -> Vec<[S; K]> {
        let mut out = vec![[S::zero(); K]; self.ncols];
        for ((r, c), d) in self.rows.iter().zip(&self.cols).zip(&self.data) {
            for a in 0..K {
                out[*c][a] += x[*r][a] * *d;
            }
        }
        out
    }

    #[must_use]
    pub fn coo(&self) -> (&[usize], &[usize], &[f64]) {
        (&self.rows, &self.cols, &self.data)
    }
}

#[must_use]
pub fn lift<S: Scalar>(x: &[f64]) -> Vec<S> {
    x.iter().map(|v| S::from_f64(*v)).collect()
}

#[must_use]
pub fn values<S: Scalar>(x: &[S]) -> Vec<f64> {
    x.iter().map(Scalar::value).collect()
}
