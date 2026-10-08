// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use faer::linalg::matmul::matmul;
use faer::linalg::triangular_solve::{
    solve_lower_triangular_in_place, solve_unit_lower_triangular_in_place,
    solve_unit_upper_triangular_in_place, solve_upper_triangular_in_place,
};
use faer::perm::PermRef;
use faer::reborrow::{Reborrow, ReborrowMut};
use faer::sparse::SymbolicSparseColMatRef;
use faer::sparse::linalg::SupernodalThreshold;
use faer::sparse::linalg::cholesky::{
    CholeskySymbolicParams, SymbolicCholeskyRaw, SymmetricOrdering, factorize_symbolic_cholesky,
};
use faer::{Accum, MatMut, MatRef, Par, Side};

use std::cell::RefCell;

use rayon::prelude::*;

use crate::error::LinalgError;
use crate::ordering::{SymmetricPattern, approximate_minimum_degree, nested_dissection};
use crate::sparse::{CscMatrix, CsrMatrix};

pub const PIVOT_THRESHOLD: f64 = 0.01;
pub const PANEL: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymmetricOrder {
    NestedDissection,
    MinimumDegree,
}

impl SymmetricOrder {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::NestedDissection => "NESTED_DISSECTION",
            Self::MinimumDegree => "MINIMUM_DEGREE_AT_PLUS_A",
        }
    }
}

#[derive(Clone, Debug)]
pub struct MultifrontalSymbolic {
    n: usize,
    order: SymmetricOrder,
    elimination: Vec<usize>,
    position: Vec<usize>,
    supernode_ptr: Vec<usize>,
    pattern_ptr: Vec<usize>,
    pattern: Vec<usize>,
    parent: Vec<usize>,
    children_ptr: Vec<usize>,
    children: Vec<usize>,
    predicted_entries: usize,
    subtree_flops: Vec<f64>,
}

impl MultifrontalSymbolic {


    pub fn analyze(a: &CscMatrix, order: SymmetricOrder) -> Result<Self, LinalgError> {
        Self::analyze_pattern(&SymmetricPattern::of(a)?, order)
    }



    #[allow(clippy::too_many_lines)]
    pub fn analyze_pattern(pattern: &SymmetricPattern, order: SymmetricOrder) -> Result<Self, LinalgError> {
        let n = pattern.n();
        let fill_reducing = match order {
            SymmetricOrder::NestedDissection => nested_dissection(pattern)?,
            SymmetricOrder::MinimumDegree => approximate_minimum_degree(pattern)?,
        };
        let elimination = postordered(pattern, &fill_reducing);
        let mut position = vec![0usize; n];
        for (k, &v) in elimination.iter().enumerate() {
            position[v] = k;
        }
        if n == 0 {
            return Ok(Self {
                n,
                order,
                elimination,
                position,
                supernode_ptr: vec![0],
                pattern_ptr: vec![0],
                pattern: Vec::new(),
                parent: Vec::new(),
                children_ptr: vec![0],
                children: Vec::new(),
                predicted_entries: 0,
                subtree_flops: Vec::new(),
            });
        }
        let sym = SymbolicSparseColMatRef::new_checked(n, n, pattern.ptr(), None, pattern.idx());
        let params = CholeskySymbolicParams {
            supernodal_flop_ratio_threshold: SupernodalThreshold::FORCE_SUPERNODAL,
            ..CholeskySymbolicParams::default()
        };
        let symbolic = factorize_symbolic_cholesky(
            sym,
            Side::Lower,
            SymmetricOrdering::Custom(PermRef::new_checked(&elimination, &position, n)),
            params,
        )?;
        let SymbolicCholeskyRaw::Supernodal(sn) = symbolic.raw() else {
            return Err(LinalgError::Invalid("supernodal analysis was not produced".into()));
        };
        let ns = sn.n_supernodes();
        let mut supernode_ptr = Vec::with_capacity(ns + 1);
        supernode_ptr.extend_from_slice(sn.supernode_begin());
        supernode_ptr.push(n);
        let mut owner = vec![0usize; n];
        for s in 0..ns {
            owner[supernode_ptr[s]..supernode_ptr[s + 1]].fill(s);
        }
        let mut pattern_ptr = Vec::with_capacity(ns + 1);
        pattern_ptr.push(0);
        let mut below = Vec::new();
        let mut parent = vec![usize::MAX; ns];
        for (s, up) in parent.iter_mut().enumerate() {
            let rows = sn.supernode(s).pattern();
            if let Some(&first) = rows.first() {
                *up = owner[first];
            }
            below.extend(rows.iter().map(|&k| elimination[k]));
            pattern_ptr.push(below.len());
        }
        let mut children_ptr = vec![0usize; ns + 1];
        for &p in &parent {
            if p != usize::MAX {
                children_ptr[p + 1] += 1;
            }
        }
        for s in 0..ns {
            children_ptr[s + 1] += children_ptr[s];
        }
        let mut next = children_ptr.clone();
        let mut children = vec![0usize; children_ptr[ns]];
        for (s, &p) in parent.iter().enumerate() {
            if p != usize::MAX {
                children[next[p]] = s;
                next[p] += 1;
            }
        }
        let mut out = Self {
            n,
            order,
            elimination,
            position,
            supernode_ptr,
            pattern_ptr,
            pattern: below,
            parent,
            children_ptr,
            children,
            predicted_entries: 0,
            subtree_flops: Vec::new(),
        };
        out.validate_tree()?;
        let mut subtree_flops = vec![0.0f64; ns];
        let mut predicted = 0usize;
        for s in 0..ns {
            let p = out.supernode_ptr[s + 1] - out.supernode_ptr[s];
            let m = p + out.structure(s).len();
            predicted += m * p + p * (m - p);
            #[allow(clippy::cast_precision_loss)]
            let own_work = (p as f64) * (m as f64) * (m as f64);
            subtree_flops[s] += own_work;
            if out.parent[s] != usize::MAX {
                let w = subtree_flops[s];
                subtree_flops[out.parent[s]] += w;
            }
        }
        out.predicted_entries = predicted;
        out.subtree_flops = subtree_flops;
        Ok(out)
    }

    fn validate_tree(&self) -> Result<(), LinalgError> {
        let mut mark = vec![usize::MAX; self.n];
        for p in 0..self.supernodes() {
            for &v in self.own(p).iter().chain(self.structure(p)) {
                mark[v] = p;
            }
            for &c in self.children_of(p) {
                if self.structure(c).iter().any(|&v| mark[v] != p) {
                    return Err(LinalgError::Invalid("inconsistent supernodal assembly tree".into()));
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn n(&self) -> usize {
        self.n
    }

    #[must_use]
    pub fn order(&self) -> SymmetricOrder {
        self.order
    }

    #[must_use]
    pub fn supernodes(&self) -> usize {
        self.supernode_ptr.len() - 1
    }

    #[must_use]
    pub fn predicted_entries(&self) -> usize {
        self.predicted_entries
    }

    #[must_use]
    pub fn nbytes(&self) -> usize {
        let words = self.elimination.len()
            + self.position.len()
            + self.supernode_ptr.len()
            + self.pattern_ptr.len()
            + self.pattern.len()
            + self.parent.len()
            + self.children_ptr.len()
            + self.children.len()
            + self.subtree_flops.len();
        words * size_of::<usize>()
    }

    fn own(&self, s: usize) -> &[usize] {
        &self.elimination[self.supernode_ptr[s]..self.supernode_ptr[s + 1]]
    }

    fn structure(&self, s: usize) -> &[usize] {
        &self.pattern[self.pattern_ptr[s]..self.pattern_ptr[s + 1]]
    }

    fn children_of(&self, s: usize) -> &[usize] {
        &self.children[self.children_ptr[s]..self.children_ptr[s + 1]]
    }




    pub fn factor(&self, a: &CscMatrix) -> Result<MultifrontalLu, LinalgError> {
        let n = self.n;
        if a.shape() != (n, n) {
            return Err(LinalgError::Shape("matrix order differs from the symbolic analysis".into()));
        }
        if !a.is_finite() {
            return Err(LinalgError::NonFinite("matrix entries must be finite".into()));
        }
        let rows_of = a.to_csr();
        let job = FactorJob { sym: self, a, rows_of: &rows_of };
        let ns = self.supernodes();
        let roots: Vec<usize> = (0..ns).filter(|&s| self.parent[s] == usize::MAX).collect();
        let outputs: Vec<Result<Subtree, LinalgError>> = roots.par_iter().map(|&r| job.subtree(r)).collect();
        let mut slots: Vec<Option<Front>> = (0..ns).map(|_| None).collect();
        let (mut delayed, mut largest) = (0usize, 0usize);
        for out in outputs {
            let out = out?;
            delayed += out.delayed;
            largest = largest.max(out.largest);
            for (s, front) in out.fronts {
                slots[s] = Some(front);
            }
        }
        let fronts: Vec<Front> = slots
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| LinalgError::Invalid("a front of the assembly tree was not factored".into()))?;
        let bytes = fronts.iter().map(Front::bytes).sum::<usize>() + size_of::<Front>() * fronts.len();
        Ok(MultifrontalLu { n, fronts, delayed, largest_front: largest, bytes })
    }
}

fn postordered(p: &SymmetricPattern, order: &[usize]) -> Vec<usize> {
    let n = order.len();
    let mut position = vec![0usize; n];
    for (k, &v) in order.iter().enumerate() {
        position[v] = k;
    }

    let mut parent = vec![usize::MAX; n];
    let mut ancestor = vec![usize::MAX; n];
    for (k, &v) in order.iter().enumerate() {
        for &u in &p.idx()[p.ptr()[v]..p.ptr()[v + 1]] {
            let mut i = position[u];
            if i >= k {
                continue;
            }
            while ancestor[i] != usize::MAX && ancestor[i] != k {
                let next = ancestor[i];
                ancestor[i] = k;
                i = next;
            }
            if ancestor[i] == usize::MAX {
                ancestor[i] = k;
                parent[i] = k;
            }
        }
    }
    let mut head = vec![usize::MAX; n];
    let mut next = vec![usize::MAX; n];

    for k in (0..n).rev() {
        if parent[k] != usize::MAX {
            next[k] = head[parent[k]];
            head[parent[k]] = k;
        }
    }
    let mut post = Vec::with_capacity(n);
    let mut stack: Vec<usize> = Vec::new();
    for root in (0..n).filter(|&k| parent[k] == usize::MAX) {
        stack.push(root);
        while let Some(&k) = stack.last() {
            if head[k] == usize::MAX {
                stack.pop();
                post.push(order[k]);
            } else {
                let child = head[k];
                head[k] = next[child];
                stack.push(child);
            }
        }
    }
    post
}

pub const PARALLEL_SUBTREE_FLOPS: f64 = 2.0e7;

thread_local! {
    static POSITIONS: RefCell<(Vec<usize>, Vec<usize>)> = const { RefCell::new((Vec::new(), Vec::new())) };
}

struct Subtree {
    fronts: Vec<(usize, Front)>,
    contribution: Option<Contribution>,
    delayed: usize,
    largest: usize,
}

impl Subtree {
    fn new() -> Self {
        Self { fronts: Vec::new(), contribution: None, delayed: 0, largest: 0 }
    }

    fn push(&mut self, s: usize, done: FrontResult) {
        self.delayed += done.delayed;
        self.largest = self.largest.max(done.front.m());
        self.fronts.push((s, done.front));
    }
}

struct FrontResult {
    front: Front,
    contribution: Option<Contribution>,
    delayed: usize,
}

struct FactorJob<'a> {
    sym: &'a MultifrontalSymbolic,
    a: &'a CscMatrix,
    rows_of: &'a CsrMatrix,
}

impl FactorJob<'_> {
    fn subtree(&self, s: usize) -> Result<Subtree, LinalgError> {
        if self.sym.subtree_flops[s] < PARALLEL_SUBTREE_FLOPS {
            return self.sequential(s);
        }
        let children = self.sym.children_of(s);
        let parts: Vec<Result<Subtree, LinalgError>> =
            children.par_iter().map(|&c| self.subtree(c)).collect();
        let mut out = Subtree::new();
        let mut blocks = Vec::with_capacity(children.len());
        for part in parts {
            let part = part?;
            out.delayed += part.delayed;
            out.largest = out.largest.max(part.largest);
            out.fronts.extend(part.fronts);
            blocks.push(part.contribution);
        }
        let mut done = self.front(s, blocks)?;
        out.contribution = done.contribution.take();
        out.push(s, done);
        Ok(out)
    }

    fn sequential(&self, root: usize) -> Result<Subtree, LinalgError> {
        let mut out = Subtree::new();
        let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
        let mut blocks: Vec<Option<Contribution>> = Vec::new();
        while let Some(top) = stack.last_mut() {
            let (v, next) = *top;
            let children = self.sym.children_of(v);
            if next < children.len() {
                top.1 += 1;
                stack.push((children[next], 0));
                continue;
            }
            stack.pop();
            let mine = blocks.split_off(blocks.len() - children.len());
            let mut done = self.front(v, mine)?;
            blocks.push(done.contribution.take());
            out.push(v, done);
        }
        out.contribution = blocks.pop().flatten();
        Ok(out)
    }

    fn front(&self, s: usize, children: Vec<Option<Contribution>>) -> Result<FrontResult, LinalgError> {
        let sym = self.sym;
        let own = sym.own(s);
        let mut rows: Vec<usize> = own.to_vec();
        let mut cols: Vec<usize> = own.to_vec();
        for cb in children.iter().flatten() {
            rows.extend_from_slice(&cb.rows[..cb.delayed]);
            cols.extend_from_slice(&cb.cols[..cb.delayed]);
        }
        let fully_summed = rows.len();
        rows.extend_from_slice(sym.structure(s));
        cols.extend_from_slice(sym.structure(s));
        let m = rows.len();
        let mut f = vec![0.0; m * m];
        POSITIONS.with(|cell| {
            let mut maps = cell.borrow_mut();
            let (row_pos, col_pos) = &mut *maps;
            if row_pos.len() < sym.n {
                row_pos.resize(sym.n, usize::MAX);
                col_pos.resize(sym.n, usize::MAX);
            }
            for (k, (&r, &c)) in rows.iter().zip(&cols).enumerate() {
                row_pos[r] = k;
                col_pos[c] = k;
            }
            let assembled = self.assemble(s, &mut f, m, row_pos, col_pos, &children);
            for (&r, &c) in rows.iter().zip(&cols) {
                row_pos[r] = usize::MAX;
                col_pos[c] = usize::MAX;
            }
            assembled
        })?;
        drop(children);
        let npiv = partial_factor(&mut f, m, fully_summed, &mut rows, &mut cols);
        let root = sym.parent[s] == usize::MAX;
        if npiv < fully_summed && root {
            return Err(LinalgError::Singular("Factor is exactly singular".into()));
        }
        let q = m - npiv;
        let mut u = vec![0.0; npiv * q];
        let mut values = Vec::with_capacity(if root { 0 } else { q * q });
        for j in 0..q {
            let col = &f[(npiv + j) * m..(npiv + j + 1) * m];
            u[j * npiv..(j + 1) * npiv].copy_from_slice(&col[..npiv]);
            if !root {
                values.extend_from_slice(&col[npiv..]);
            }
        }

        f.truncate(m * npiv);
        f.shrink_to_fit();
        let l = f;
        let contribution = (!root && q > 0).then(|| Contribution {
            rows: rows[npiv..].to_vec(),
            cols: cols[npiv..].to_vec(),
            delayed: fully_summed - npiv,
            values,
        });
        rows.shrink_to_fit();
        cols.shrink_to_fit();
        Ok(FrontResult {
            front: Front { rows, cols, npiv, l, u },
            contribution,
            delayed: fully_summed - npiv,
        })
    }

    fn assemble(
        &self,
        s: usize,
        f: &mut [f64],
        m: usize,
        row_pos: &[usize],
        col_pos: &[usize],
        children: &[Option<Contribution>],
    ) -> Result<(), LinalgError> {
        let sym = self.sym;
        let (cp, ci, cv) = (self.a.indptr(), self.a.indices(), self.a.data());
        let (rp, ri, rv) = (self.rows_of.indptr(), self.rows_of.indices(), self.rows_of.data());
        let (begin, end) = (sym.supernode_ptr[s], sym.supernode_ptr[s + 1]);
        let outside = || LinalgError::Invalid("entry outside its front".into());

        for (k, &j) in sym.own(s).iter().enumerate() {
            for (&i, &v) in ci[cp[j]..cp[j + 1]].iter().zip(&cv[cp[j]..cp[j + 1]]) {
                if sym.position[i] >= begin {
                    let r = row_pos[i];
                    if r == usize::MAX {
                        return Err(outside());
                    }
                    f[k * m + r] += v;
                }
            }
            for (&i, &v) in ri[rp[j]..rp[j + 1]].iter().zip(&rv[rp[j]..rp[j + 1]]) {
                if sym.position[i] >= end {
                    let c = col_pos[i];
                    if c == usize::MAX {
                        return Err(outside());
                    }
                    f[c * m + k] += v;
                }
            }
        }

        let mut finite = true;
        let mut local: Vec<usize> = Vec::new();
        for cb in children.iter().flatten() {
            let q = cb.rows.len();

            local.clear();
            for &r in &cb.rows {
                let lr = row_pos[r];
                if lr == usize::MAX {
                    return Err(outside());
                }
                local.push(lr);
            }
            for (jj, &cj) in cb.cols.iter().enumerate() {
                let lc = col_pos[cj];
                if lc == usize::MAX {
                    return Err(outside());
                }
                let dst = &mut f[lc * m..(lc + 1) * m];
                for (&lr, &v) in local.iter().zip(&cb.values[jj * q..(jj + 1) * q]) {
                    finite &= v.is_finite();
                    dst[lr] += v;
                }
            }
        }
        if !finite {
            return Err(LinalgError::NonFinite("matrix entries must be finite".into()));
        }
        Ok(())
    }
}

struct Contribution {
    rows: Vec<usize>,
    cols: Vec<usize>,
    delayed: usize,
    values: Vec<f64>,
}

#[derive(Clone, Debug)]
struct Front {
    rows: Vec<usize>,
    cols: Vec<usize>,
    npiv: usize,
    l: Vec<f64>,
    u: Vec<f64>,
}

impl Front {
    fn bytes(&self) -> usize {
        (self.l.len() + self.u.len()) * size_of::<f64>()
            + (self.rows.len() + self.cols.len()) * size_of::<usize>()
    }

    fn m(&self) -> usize {
        self.rows.len()
    }

    fn lu11(&self) -> MatRef<'_, f64> {
        MatRef::from_column_major_slice(&self.l, self.m(), self.npiv).submatrix(0, 0, self.npiv, self.npiv)
    }

    fn l21(&self) -> MatRef<'_, f64> {
        let m = self.m();
        MatRef::from_column_major_slice(&self.l, m, self.npiv).submatrix(
            self.npiv,
            0,
            m - self.npiv,
            self.npiv,
        )
    }

    fn u12(&self) -> MatRef<'_, f64> {
        MatRef::from_column_major_slice(&self.u, self.npiv, self.m() - self.npiv)
    }
}

#[derive(Clone, Debug)]
pub struct MultifrontalLu {
    n: usize,
    fronts: Vec<Front>,
    delayed: usize,
    largest_front: usize,
    bytes: usize,
}

impl MultifrontalLu {
    #[must_use]
    pub fn n(&self) -> usize {
        self.n
    }

    #[must_use]
    pub fn nbytes(&self) -> usize {
        self.bytes
    }

    #[must_use]
    pub fn delayed_pivots(&self) -> usize {
        self.delayed
    }

    #[must_use]
    pub fn largest_front(&self) -> usize {
        self.largest_front
    }

    #[must_use]
    pub fn entries(&self) -> usize {
        self.fronts.iter().map(|f| f.l.len() + f.u.len()).sum()
    }



    pub fn solve_in_place(&self, b: &mut [f64], ncols: usize, transpose: bool) -> Result<(), LinalgError> {
        let n = self.n;
        if b.len() != n * ncols {
            return Err(LinalgError::Shape(format!(
                "right-hand side of length {} for n = {n}, {ncols} columns",
                b.len()
            )));
        }
        if n == 0 || ncols == 0 {
            return Ok(());
        }
        let mut out = vec![0.0; n * ncols];
        let mut z = Vec::new();
        let mut t = Vec::new();

        for f in &self.fronts {
            let (p, q) = (f.npiv, f.m() - f.npiv);
            let (piv, rest) = if transpose { (&f.cols, &f.cols) } else { (&f.rows, &f.rows) };
            gather(b, n, ncols, &piv[..p], &mut z);
            let mut zm = MatMut::from_column_major_slice_mut(&mut z, p, ncols);
            if transpose {
                solve_lower_triangular_in_place(f.lu11().transpose(), zm.rb_mut(), Par::Seq);
            } else {
                solve_unit_lower_triangular_in_place(f.lu11(), zm.rb_mut(), Par::Seq);
            }
            scatter(&z, n, ncols, &piv[..p], b);
            if q > 0 {
                t.clear();
                t.resize(q * ncols, 0.0);
                let tm = MatMut::from_column_major_slice_mut(&mut t, q, ncols);
                let zr = MatRef::from_column_major_slice(&z, p, ncols);
                if transpose {
                    matmul(tm, Accum::Replace, f.u12().transpose(), zr, 1.0, Par::Seq);
                } else {
                    matmul(tm, Accum::Replace, f.l21(), zr, 1.0, Par::Seq);
                }
                for c in 0..ncols {
                    let col = &mut b[c * n..(c + 1) * n];
                    for (&r, &v) in rest[p..].iter().zip(&t[c * q..(c + 1) * q]) {
                        col[r] -= v;
                    }
                }
            }
        }
        for f in self.fronts.iter().rev() {
            let (p, q) = (f.npiv, f.m() - f.npiv);
            let (piv, other) = if transpose { (&f.cols, &f.rows) } else { (&f.rows, &f.cols) };
            gather(b, n, ncols, &piv[..p], &mut z);
            if q > 0 {
                gather(&out, n, ncols, &other[p..], &mut t);
                let tr = MatRef::from_column_major_slice(&t, q, ncols);
                let zm = MatMut::from_column_major_slice_mut(&mut z, p, ncols);
                if transpose {
                    matmul(zm, Accum::Add, f.l21().transpose(), tr, -1.0, Par::Seq);
                } else {
                    matmul(zm, Accum::Add, f.u12(), tr, -1.0, Par::Seq);
                }
            }
            let mut zm = MatMut::from_column_major_slice_mut(&mut z, p, ncols);
            if transpose {
                solve_unit_upper_triangular_in_place(f.lu11().transpose(), zm.rb_mut(), Par::Seq);
            } else {
                solve_upper_triangular_in_place(f.lu11(), zm.rb_mut(), Par::Seq);
            }
            let target = if transpose { &f.rows } else { &f.cols };
            scatter(&z, n, ncols, &target[..p], &mut out);
        }
        b.copy_from_slice(&out);
        Ok(())
    }
}

fn gather(src: &[f64], n: usize, ncols: usize, idx: &[usize], out: &mut Vec<f64>) {
    out.clear();
    out.reserve(idx.len() * ncols);
    for c in 0..ncols {
        let col = &src[c * n..(c + 1) * n];
        out.extend(idx.iter().map(|&i| col[i]));
    }
}

fn scatter(values: &[f64], n: usize, ncols: usize, idx: &[usize], dst: &mut [f64]) {
    let k = idx.len();
    for c in 0..ncols {
        let col = &mut dst[c * n..(c + 1) * n];
        for (&i, &v) in idx.iter().zip(&values[c * k..(c + 1) * k]) {
            col[i] = v;
        }
    }
}

fn swap_rows(f: &mut [f64], m: usize, a: usize, b: usize) {
    if a != b {
        for col in f.chunks_exact_mut(m) {
            col.swap(a, b);
        }
    }
}

fn swap_cols(f: &mut [f64], m: usize, a: usize, b: usize) {
    if a != b {
        let (lo, hi) = (a.min(b), a.max(b));
        let (left, right) = f.split_at_mut(hi * m);
        left[lo * m..(lo + 1) * m].swap_with_slice(&mut right[..m]);
    }
}

fn catch_up(f: &mut [f64], m: usize, p0: usize, k: usize, j: usize) {
    let (left, right) = f.split_at_mut(j * m);
    let col = &mut right[..m];
    for t in p0..k {
        let u = col[t];
        if u != 0.0 {
            let l = &left[t * m..(t + 1) * m];
            for (x, &li) in col[t + 1..].iter_mut().zip(&l[t + 1..]) {
                *x -= li * u;
            }
        }
    }
}

fn partial_factor(f: &mut [f64], m: usize, fs: usize, rows: &mut [usize], cols: &mut [usize]) -> usize {
    let mut k = 0usize;
    let mut stale = vec![false; fs];
    while k < fs {
        let p0 = k;

        let mut w1 = k;
        stale[k..].fill(false);
        while k - p0 < PANEL {
            let c = if let Some(c) = (k..w1).find(|&c| !stale[c]) {
                c
            } else {
                if w1 == fs {
                    break;
                }
                catch_up(f, m, p0, k, w1);
                w1 += 1;
                w1 - 1
            };
            let col = &f[c * m..(c + 1) * m];
            let column_max = col[k..].iter().fold(0.0f64, |a, v| a.max(v.abs()));
            let mut r = k;
            let mut best = 0.0f64;
            for (i, v) in col[k..fs].iter().enumerate() {
                if v.abs() > best {
                    best = v.abs();
                    r = k + i;
                }
            }
            if best > 0.0 && best >= PIVOT_THRESHOLD * column_max {
                swap_rows(f, m, r, k);
                rows.swap(r, k);
                swap_cols(f, m, c, k);
                cols.swap(c, k);
                stale.swap(c, k);
                let pivot = f[k * m + k];
                for x in &mut f[k * m + k + 1..(k + 1) * m] {
                    *x /= pivot;
                }
                let (left, right) = f.split_at_mut((k + 1) * m);
                let l = &left[k * m..];
                for j in 0..w1 - k - 1 {
                    let col = &mut right[j * m..(j + 1) * m];
                    let u = col[k];
                    if u != 0.0 {
                        for (x, &li) in col[k + 1..].iter_mut().zip(&l[k + 1..]) {
                            *x -= li * u;
                        }
                    }
                }
                k += 1;
                stale[k..w1].fill(false);
            } else {
                stale[c] = true;
            }
        }
        if k == p0 {
            break;
        }
        if w1 < m {
            let fm = MatMut::from_column_major_slice_mut(f, m, m);
            let (left, right) = fm.split_at_col_mut(w1);
            let (mut top, bottom) = right.split_at_row_mut(k);
            let mut u12 = top.rb_mut().submatrix_mut(p0, 0, k - p0, m - w1);
            solve_unit_lower_triangular_in_place(
                left.rb().submatrix(p0, p0, k - p0, k - p0),
                u12.rb_mut(),
                Par::Seq,
            );

            #[allow(clippy::cast_precision_loss)]
            let flops = (m - k) as f64 * (m - w1) as f64 * (k - p0) as f64;
            let par = if flops >= PARALLEL_UPDATE_FLOPS { Par::rayon(0) } else { Par::Seq };
            matmul(bottom, Accum::Add, left.rb().submatrix(k, p0, m - k, k - p0), u12.rb(), -1.0, par);
        }
    }
    k
}

pub const PARALLEL_UPDATE_FLOPS: f64 = 4.0e7;

