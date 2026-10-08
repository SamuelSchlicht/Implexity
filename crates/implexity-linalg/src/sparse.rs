// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use rayon::prelude::*;

use crate::error::LinalgError;

#[derive(Clone, Debug, PartialEq)]
struct Compressed {
    major: usize,
    minor: usize,
    ptr: Vec<usize>,
    idx: Vec<usize>,
    data: Vec<f64>,
}

impl Compressed {
    fn try_new(
        major: usize,
        minor: usize,
        ptr: Vec<usize>,
        idx: Vec<usize>,
        data: Vec<f64>,
    ) -> Result<Self, LinalgError> {
        if major.checked_add(1) != Some(ptr.len()) || ptr.first() != Some(&0) {
            return Err(LinalgError::Shape(format!(
                "pointer array of length {} for {major} slices",
                ptr.len()
            )));
        }
        if *ptr.last().unwrap_or(&0) != idx.len() || idx.len() != data.len() {
            return Err(LinalgError::Shape(format!(
                "pointer end {} with {} indices and {} values",
                ptr.last().unwrap_or(&0),
                idx.len(),
                data.len()
            )));
        }
        if ptr.iter().any(|&p| p > idx.len()) {
            return Err(LinalgError::Shape("pointer exceeds index array length".into()));
        }
        for s in 0..major {
            let (a, b) = (ptr[s], ptr[s + 1]);
            if a > b {
                return Err(LinalgError::Shape(format!("decreasing pointer at slice {s}")));
            }
            let slice = &idx[a..b];
            if slice.iter().any(|&i| i >= minor) {
                return Err(LinalgError::Shape(format!("index out of range {minor} in slice {s}")));
            }
            if slice.windows(2).any(|w| w[0] >= w[1]) {
                return Err(LinalgError::Shape(format!("indices of slice {s} are not strictly increasing")));
            }
        }
        Ok(Self { major, minor, ptr, idx, data })
    }

    fn from_coordinates(
        major: usize,
        minor: usize,
        maj: &[usize],
        min: &[usize],
        vals: &[f64],
    ) -> Result<Self, LinalgError> {
        let plan = PatternPlan::new(major, minor, maj, min)?;
        let data = plan.reduce(vals)?;
        Ok(Self { major, minor, ptr: plan.ptr, idx: plan.idx, data })
    }

    fn get(&self, i: usize, j: usize) -> f64 {
        if i >= self.major {
            return 0.0;
        }
        let s = &self.idx[self.ptr[i]..self.ptr[i + 1]];
        s.binary_search(&j).map_or(0.0, |k| self.data[self.ptr[i] + k])
    }

    fn slice_matvec(&self, x: &[f64], y: &mut [f64]) {
        y.par_iter_mut().enumerate().for_each(|(i, yi)| {
            let mut acc = 0.0;
            for k in self.ptr[i]..self.ptr[i + 1] {
                acc += self.data[k] * x[self.idx[k]];
            }
            *yi = acc;
        });
    }

    fn scatter_matvec(&self, x: &[f64], y: &mut [f64]) {
        y.fill(0.0);
        for (s, &xs) in x.iter().enumerate() {
            for k in self.ptr[s]..self.ptr[s + 1] {
                y[self.idx[k]] += self.data[k] * xs;
            }
        }
    }

    fn transposed_storage(&self) -> Self {
        let mut count = vec![0usize; self.minor + 1];
        for &j in &self.idx {
            count[j + 1] += 1;
        }
        for j in 0..self.minor {
            count[j + 1] += count[j];
        }
        let ptr = count.clone();
        let mut next = count;
        let mut idx = vec![0; self.idx.len()];
        let mut data = vec![0.0; self.data.len()];
        for s in 0..self.major {
            for k in self.ptr[s]..self.ptr[s + 1] {
                let j = self.idx[k];
                let dst = next[j];
                idx[dst] = s;
                data[dst] = self.data[k];
                next[j] += 1;
            }
        }
        Self { major: self.minor, minor: self.major, ptr, idx, data }
    }
}

struct PatternPlan {
    ptr: Vec<usize>,
    idx: Vec<usize>,
    slots: Vec<usize>,
}

impl PatternPlan {
    fn new(major: usize, minor: usize, maj: &[usize], min: &[usize]) -> Result<Self, LinalgError> {
        if maj.len() != min.len() {
            return Err(LinalgError::Shape(format!(
                "{} row and {} column coordinates",
                maj.len(),
                min.len()
            )));
        }
        if let Some(bad) = maj.iter().position(|&i| i >= major) {
            return Err(LinalgError::Shape(format!("coordinate {} out of range {major}", maj[bad])));
        }
        if let Some(bad) = min.iter().position(|&j| j >= minor) {
            return Err(LinalgError::Shape(format!("coordinate {} out of range {minor}", min[bad])));
        }

        let mut count = vec![0usize; major + 1];
        for &i in maj {
            count[i + 1] += 1;
        }
        for i in 0..major {
            count[i + 1] += count[i];
        }
        let mut order = vec![0usize; maj.len()];
        let mut next = count.clone();
        for (e, &i) in maj.iter().enumerate() {
            order[next[i]] = e;
            next[i] += 1;
        }
        let mut ptr = Vec::with_capacity(major + 1);
        ptr.push(0);
        let mut idx = Vec::new();
        let mut slots = vec![0usize; maj.len()];
        for i in 0..major {
            let bucket = &mut order[count[i]..count[i + 1]];
            bucket.sort_by_key(|&e| (min[e], e));
            let mut last = None;
            for &e in bucket.iter() {
                if last != Some(min[e]) {
                    idx.push(min[e]);
                    last = Some(min[e]);
                }
                slots[e] = idx.len() - 1;
            }
            ptr.push(idx.len());
        }
        Ok(Self { ptr, idx, slots })
    }

    fn reduce(&self, vals: &[f64]) -> Result<Vec<f64>, LinalgError> {
        if vals.len() != self.slots.len() {
            return Err(LinalgError::Shape(format!(
                "{} values for {} entries",
                vals.len(),
                self.slots.len()
            )));
        }
        let mut data = vec![0.0; self.idx.len()];
        for (&s, &v) in self.slots.iter().zip(vals) {
            data[s] += v;
        }
        Ok(data)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CsrMatrix(Compressed);

#[derive(Clone, Debug, PartialEq)]
pub struct CscMatrix(Compressed);

macro_rules! common_impl {
    ($t:ident, $major:literal, $minor:literal) => {
        impl $t {
            #[must_use]
            pub fn nnz(&self) -> usize {
                self.0.data.len()
            }

            #[must_use]
            pub fn indptr(&self) -> &[usize] {
                &self.0.ptr
            }

            #[must_use]
            pub fn indices(&self) -> &[usize] {
                &self.0.idx
            }

            #[must_use]
            pub fn data(&self) -> &[f64] {
                &self.0.data
            }

            pub fn data_mut(&mut self) -> &mut [f64] {
                &mut self.0.data
            }

            #[must_use]
            pub fn into_parts(self) -> (Vec<usize>, Vec<usize>, Vec<f64>) {
                (self.0.ptr, self.0.idx, self.0.data)
            }

            #[must_use]
            pub fn same_pattern(&self, other: &Self) -> bool {
                self.0.major == other.0.major
                    && self.0.minor == other.0.minor
                    && self.0.ptr == other.0.ptr
                    && self.0.idx == other.0.idx
            }



            pub fn with_data(&self, data: Vec<f64>) -> Result<Self, LinalgError> {
                if data.len() != self.nnz() {
                    return Err(LinalgError::Shape(format!(
                        "{} values for {} stored entries",
                        data.len(),
                        self.nnz()
                    )));
                }
                Ok(Self(Compressed { data, ..self.0.clone() }))
            }

            #[must_use]
            pub fn is_finite(&self) -> bool {
                self.0.data.iter().all(|v| v.is_finite())
            }

            #[must_use]
            pub fn nbytes(&self) -> usize {
                (self.0.ptr.len() + self.0.idx.len()) * size_of::<usize>()
                    + self.0.data.len() * size_of::<f64>()
            }

            #[must_use]
            pub fn norm_fro(&self) -> f64 {
                self.0.data.iter().map(|v| v * v).sum::<f64>().sqrt()
            }
        }
    };
}
common_impl!(CsrMatrix, "row", "column");
common_impl!(CscMatrix, "column", "row");

impl CsrMatrix {


    pub fn try_new(
        nrows: usize,
        ncols: usize,
        indptr: Vec<usize>,
        indices: Vec<usize>,
        data: Vec<f64>,
    ) -> Result<Self, LinalgError> {
        Compressed::try_new(nrows, ncols, indptr, indices, data).map(Self)
    }



    pub fn from_triplets(
        nrows: usize,
        ncols: usize,
        rows: &[usize],
        cols: &[usize],
        vals: &[f64],
    ) -> Result<Self, LinalgError> {
        if vals.len() != rows.len() {
            return Err(LinalgError::Shape(format!("{} values for {} coordinates", vals.len(), rows.len())));
        }
        Compressed::from_coordinates(nrows, ncols, rows, cols, vals).map(Self)
    }



    pub fn from_dense(nrows: usize, ncols: usize, dense: &[f64]) -> Result<Self, LinalgError> {
        if dense.len() != nrows * ncols {
            return Err(LinalgError::Shape(format!("{} entries for {nrows}×{ncols}", dense.len())));
        }
        let mut ptr = vec![0];
        let mut idx = Vec::new();
        let mut data = Vec::new();
        for i in 0..nrows {
            for j in 0..ncols {
                let v = dense[i * ncols + j];
                if v != 0.0 {
                    idx.push(j);
                    data.push(v);
                }
            }
            ptr.push(idx.len());
        }
        Ok(Self(Compressed { major: nrows, minor: ncols, ptr, idx, data }))
    }

    #[must_use]
    pub fn identity(n: usize) -> Self {
        Self::diagonal_matrix(&vec![1.0; n])
    }

    #[must_use]
    pub fn diagonal_matrix(d: &[f64]) -> Self {
        let n = d.len();
        Self(Compressed {
            major: n,
            minor: n,
            ptr: (0..=n).collect(),
            idx: (0..n).collect(),
            data: d.to_vec(),
        })
    }

    #[must_use]
    pub fn nrows(&self) -> usize {
        self.0.major
    }

    #[must_use]
    pub fn ncols(&self) -> usize {
        self.0.minor
    }

    #[must_use]
    pub fn shape(&self) -> (usize, usize) {
        (self.0.major, self.0.minor)
    }

    #[must_use]
    pub fn get(&self, i: usize, j: usize) -> f64 {
        self.0.get(i, j)
    }



    #[must_use]
    pub fn row(&self, i: usize) -> (&[usize], &[f64]) {
        let (a, b) = (self.0.ptr[i], self.0.ptr[i + 1]);
        (&self.0.idx[a..b], &self.0.data[a..b])
    }



    pub fn matvec_into(&self, x: &[f64], y: &mut [f64]) -> Result<(), LinalgError> {
        if x.len() != self.ncols() || y.len() != self.nrows() {
            return Err(LinalgError::Shape(format!(
                "matvec: {}×{} times {} into {}",
                self.nrows(),
                self.ncols(),
                x.len(),
                y.len()
            )));
        }
        self.0.slice_matvec(x, y);
        Ok(())
    }



    pub fn matvec(&self, x: &[f64]) -> Result<Vec<f64>, LinalgError> {
        let mut y = vec![0.0; self.nrows()];
        self.matvec_into(x, &mut y)?;
        Ok(y)
    }



    pub fn matvec_transpose(&self, x: &[f64]) -> Result<Vec<f64>, LinalgError> {
        if x.len() != self.nrows() {
            return Err(LinalgError::Shape(format!(
                "matvecᵀ: {}×{} with {}",
                self.nrows(),
                self.ncols(),
                x.len()
            )));
        }
        let mut y = vec![0.0; self.ncols()];
        self.0.scatter_matvec(x, &mut y);
        Ok(y)
    }

    #[must_use]
    pub fn into_transpose(self) -> CscMatrix {
        CscMatrix(self.0)
    }

    #[must_use]
    pub fn transpose(&self) -> Self {
        Self(self.0.transposed_storage())
    }

    #[must_use]
    pub fn to_csc(&self) -> CscMatrix {
        CscMatrix(self.0.transposed_storage())
    }

    #[must_use]
    pub fn diagonal(&self) -> Vec<f64> {
        (0..self.nrows().min(self.ncols())).map(|i| self.get(i, i)).collect()
    }

    #[must_use]
    pub fn to_dense(&self) -> Vec<f64> {
        let n = self.ncols();
        let mut d = vec![0.0; self.nrows() * n];
        for i in 0..self.nrows() {
            let (c, v) = self.row(i);
            for (&j, &x) in c.iter().zip(v) {
                d[i * n + j] = x;
            }
        }
        d
    }

    #[must_use]
    pub fn norm_1(&self) -> f64 {
        let mut s = vec![0.0; self.ncols()];
        for (&j, &v) in self.0.idx.iter().zip(&self.0.data) {
            s[j] += v.abs();
        }
        s.into_iter().fold(0.0, f64::max)
    }

    #[must_use]
    pub fn norm_inf(&self) -> f64 {
        (0..self.nrows()).map(|i| self.row(i).1.iter().map(|v| v.abs()).sum::<f64>()).fold(0.0, f64::max)
    }



    pub fn scaled(&self, row_scale: &[f64], col_scale: &[f64]) -> Result<Self, LinalgError> {
        if row_scale.len() != self.nrows() || col_scale.len() != self.ncols() {
            return Err(LinalgError::Shape("scaling vectors do not match the matrix".into()));
        }
        let mut out = self.clone();
        for (i, &rs) in row_scale.iter().enumerate() {
            for k in self.0.ptr[i]..self.0.ptr[i + 1] {
                out.0.data[k] = rs * self.0.data[k] * col_scale[self.0.idx[k]];
            }
        }
        Ok(out)
    }



    pub fn add_scaled(&self, alpha: f64, other: &Self, beta: f64) -> Result<Self, LinalgError> {
        if self.shape() != other.shape() {
            return Err(LinalgError::Shape("add: shapes differ".into()));
        }
        let mut ptr = vec![0];
        let mut idx = Vec::with_capacity(self.nnz() + other.nnz());
        let mut data = Vec::with_capacity(self.nnz() + other.nnz());
        for i in 0..self.nrows() {
            let (ca, va) = self.row(i);
            let (cb, vb) = other.row(i);
            let (mut p, mut q) = (0, 0);
            while p < ca.len() || q < cb.len() {
                let take_a = q >= cb.len() || (p < ca.len() && ca[p] <= cb[q]);
                let take_b = p >= ca.len() || (q < cb.len() && cb[q] <= ca[p]);
                if take_a && take_b {
                    idx.push(ca[p]);
                    data.push(alpha * va[p] + beta * vb[q]);
                    p += 1;
                    q += 1;
                } else if take_a {
                    idx.push(ca[p]);
                    data.push(alpha * va[p]);
                    p += 1;
                } else {
                    idx.push(cb[q]);
                    data.push(beta * vb[q]);
                    q += 1;
                }
            }
            ptr.push(idx.len());
        }
        Ok(Self(Compressed { major: self.nrows(), minor: self.ncols(), ptr, idx, data }))
    }



    pub fn matmul(&self, other: &Self) -> Result<Self, LinalgError> {
        if self.ncols() != other.nrows() {
            return Err(LinalgError::Shape("matmul: inner dimensions differ".into()));
        }
        let m = other.ncols();
        let mut ptr = vec![0];
        let mut idx = Vec::new();
        let mut data = Vec::new();
        let mut acc = vec![0.0; m];
        let mut mark = vec![usize::MAX; m];
        let mut cols = Vec::new();
        for i in 0..self.nrows() {
            cols.clear();
            let (ca, va) = self.row(i);
            for (&k, &a) in ca.iter().zip(va) {
                let (cb, vb) = other.row(k);
                for (&j, &b) in cb.iter().zip(vb) {
                    if mark[j] != i {
                        mark[j] = i;
                        acc[j] = 0.0;
                        cols.push(j);
                    }
                    acc[j] += a * b;
                }
            }
            cols.sort_unstable();
            for &j in &cols {
                idx.push(j);
                data.push(acc[j]);
            }
            ptr.push(idx.len());
        }
        Ok(Self(Compressed { major: self.nrows(), minor: m, ptr, idx, data }))
    }
}

impl CscMatrix {


    pub fn try_new(
        nrows: usize,
        ncols: usize,
        indptr: Vec<usize>,
        indices: Vec<usize>,
        data: Vec<f64>,
    ) -> Result<Self, LinalgError> {
        Compressed::try_new(ncols, nrows, indptr, indices, data).map(Self)
    }



    pub fn from_triplets(
        nrows: usize,
        ncols: usize,
        rows: &[usize],
        cols: &[usize],
        vals: &[f64],
    ) -> Result<Self, LinalgError> {
        if vals.len() != rows.len() {
            return Err(LinalgError::Shape(format!("{} values for {} coordinates", vals.len(), rows.len())));
        }
        Compressed::from_coordinates(ncols, nrows, cols, rows, vals).map(Self)
    }

    #[must_use]
    pub fn nrows(&self) -> usize {
        self.0.minor
    }

    #[must_use]
    pub fn ncols(&self) -> usize {
        self.0.major
    }

    #[must_use]
    pub fn shape(&self) -> (usize, usize) {
        (self.0.minor, self.0.major)
    }

    #[must_use]
    pub fn get(&self, i: usize, j: usize) -> f64 {
        self.0.get(j, i)
    }



    pub fn matvec(&self, x: &[f64]) -> Result<Vec<f64>, LinalgError> {
        if x.len() != self.ncols() {
            return Err(LinalgError::Shape(format!(
                "matvec: {} columns, vector of {}",
                self.ncols(),
                x.len()
            )));
        }
        let mut y = vec![0.0; self.nrows()];
        self.0.scatter_matvec(x, &mut y);
        Ok(y)
    }



    pub fn matvec_transpose(&self, x: &[f64]) -> Result<Vec<f64>, LinalgError> {
        if x.len() != self.nrows() {
            return Err(LinalgError::Shape(format!("matvecᵀ: {} rows, vector of {}", self.nrows(), x.len())));
        }
        let mut y = vec![0.0; self.ncols()];
        self.0.slice_matvec(x, &mut y);
        Ok(y)
    }

    #[must_use]
    pub fn into_transpose(self) -> CsrMatrix {
        CsrMatrix(self.0)
    }

    #[must_use]
    pub fn to_csr(&self) -> CsrMatrix {
        CsrMatrix(self.0.transposed_storage())
    }



    #[must_use]
    pub fn col(&self, j: usize) -> (&[usize], &[f64]) {
        let (a, b) = (self.0.ptr[j], self.0.ptr[j + 1]);
        (&self.0.idx[a..b], &self.0.data[a..b])
    }

    #[must_use]
    pub fn as_faer(&self) -> faer::sparse::SparseColMatRef<'_, usize, f64> {
        let sym = faer::sparse::SymbolicSparseColMatRef::new_checked(
            self.nrows(),
            self.ncols(),
            &self.0.ptr,
            None,
            &self.0.idx,
        );
        faer::sparse::SparseColMatRef::new(sym, &self.0.data)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Csr,
    Csc,
}


#[derive(Clone, Debug, PartialEq)]
pub struct AssemblyPlan {
    format: Format,
    nrows: usize,
    ncols: usize,
    ptr: Vec<usize>,
    idx: Vec<usize>,
    slots: Vec<usize>,
    group_ptr: Vec<usize>,
    group: Vec<usize>,
}

impl AssemblyPlan {


    pub fn new(
        nrows: usize,
        ncols: usize,
        rows: &[usize],
        cols: &[usize],
        format: Format,
    ) -> Result<Self, LinalgError> {
        let p = match format {
            Format::Csr => PatternPlan::new(nrows, ncols, rows, cols)?,
            Format::Csc => PatternPlan::new(ncols, nrows, cols, rows)?,
        };
        let nnz = p.idx.len();
        let mut group_ptr = vec![0usize; nnz + 1];
        for &s in &p.slots {
            group_ptr[s + 1] += 1;
        }
        for s in 0..nnz {
            group_ptr[s + 1] += group_ptr[s];
        }
        let mut next = group_ptr.clone();
        let mut group = vec![0usize; p.slots.len()];
        for (e, &s) in p.slots.iter().enumerate() {
            group[next[s]] = e;
            next[s] += 1;
        }
        Ok(Self { format, nrows, ncols, ptr: p.ptr, idx: p.idx, slots: p.slots, group_ptr, group })
    }

    #[must_use]
    pub fn input_entries(&self) -> usize {
        self.slots.len()
    }

    #[must_use]
    pub fn nnz(&self) -> usize {
        self.idx.len()
    }

    #[must_use]
    pub fn slots(&self) -> &[usize] {
        &self.slots
    }

    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        (self.slots.len() + self.idx.len() + self.ptr.len() + self.group.len() + self.group_ptr.len())
            * size_of::<usize>()
    }



    pub fn assemble_into(&self, values: &[f64], data: &mut [f64]) -> Result<(), LinalgError> {
        if values.len() != self.slots.len() || data.len() != self.idx.len() {
            return Err(LinalgError::Shape(format!(
                "assembly plan for {} entries / {} slots applied to {} values / {} slots",
                self.slots.len(),
                self.idx.len(),
                values.len(),
                data.len()
            )));
        }
        data.par_iter_mut().enumerate().for_each(|(s, d)| {
            let mut acc = 0.0;
            for &e in &self.group[self.group_ptr[s]..self.group_ptr[s + 1]] {
                acc += values[e];
            }
            *d = acc;
        });
        Ok(())
    }



    pub fn assemble_csr(&self, values: &[f64]) -> Result<CsrMatrix, LinalgError> {
        if self.format != Format::Csr {
            return Err(LinalgError::Invalid("assembly plan targets CSC".into()));
        }
        let mut data = vec![0.0; self.idx.len()];
        self.assemble_into(values, &mut data)?;
        Ok(CsrMatrix(Compressed {
            major: self.nrows,
            minor: self.ncols,
            ptr: self.ptr.clone(),
            idx: self.idx.clone(),
            data,
        }))
    }



    pub fn assemble_csc(&self, values: &[f64]) -> Result<CscMatrix, LinalgError> {
        if self.format != Format::Csc {
            return Err(LinalgError::Invalid("assembly plan targets CSR".into()));
        }
        let mut data = vec![0.0; self.idx.len()];
        self.assemble_into(values, &mut data)?;
        Ok(CscMatrix(Compressed {
            major: self.ncols,
            minor: self.nrows,
            ptr: self.ptr.clone(),
            idx: self.idx.clone(),
            data,
        }))
    }
}

