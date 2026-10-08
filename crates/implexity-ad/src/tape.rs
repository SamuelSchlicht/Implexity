// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





use std::sync::Arc;

use crate::dual::Dual;
use crate::error::AdError;
use crate::scalar::Scalar;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Var {
    id: usize,
    len: usize,
}

impl Var {
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub const fn index(&self) -> usize {
        self.id
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SparseConst {
    pub nrows: usize,
    pub ncols: usize,
    pub indptr: Vec<usize>,
    pub indices: Vec<usize>,
    pub data: Vec<f64>,
}

impl SparseConst {


    pub fn new(
        nrows: usize,
        ncols: usize,
        indptr: Vec<usize>,
        indices: Vec<usize>,
        data: Vec<f64>,
    ) -> Result<Self, AdError> {
        let ok = indptr.len() == nrows + 1
            && indptr.first() == Some(&0)
            && indptr.windows(2).all(|w| w[0] <= w[1])
            && indptr.last() == Some(&indices.len())
            && indices.len() == data.len()
            && indices.iter().all(|&c| c < ncols);
        if ok {
            Ok(Self { nrows, ncols, indptr, indices, data })
        } else {
            Err(AdError::Shape("SparseConst: inconsistent CSR arrays".into()))
        }
    }
}

pub type Pullback = Box<dyn Fn(&[f64]) -> Result<Vec<Vec<f64>>, AdError> + Send + Sync>;

enum Op {
    Leaf,
    Constant,
    Scale { x: usize, c: f64 },
    Add { a: usize, b: usize },
    Sub { a: usize, b: usize },
    Mul { a: usize, b: usize },
    Div { a: usize, b: usize },
    Elementwise { args: Vec<usize>, partials: Vec<Vec<f64>> },
    Sum { x: usize },
    Dot { a: usize, b: usize },
    Gather { x: usize, idx: Vec<usize> },
    ScatterAdd { x: usize, idx: Vec<usize> },
    Concat { parts: Vec<usize> },
    Slice { x: usize, start: usize },
    DenseMatVec { x: usize, m: Arc<Vec<f64>>, rows: usize, cols: usize },
    SparseMatVec { x: usize, m: Arc<SparseConst> },
    Custom { inputs: Vec<usize>, pullback: Pullback },
}

struct Node {
    value: Vec<f64>,
    op: Op,
}

#[derive(Default)]
pub struct Tape {
    nodes: Vec<Node>,
}

impl core::fmt::Debug for Tape {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Tape").field("nodes", &self.nodes.len()).finish()
    }
}

#[derive(Debug, Clone)]
pub struct Grads {
    grads: Vec<Option<Vec<f64>>>,
    lens: Vec<usize>,
}

impl Grads {


    pub fn wrt(&self, v: Var) -> Result<Vec<f64>, AdError> {
        match self.lens.get(v.id) {
            Some(&len) if len == v.len => Ok(self.grads[v.id].clone().unwrap_or_else(|| vec![0.0; len])),
            _ => Err(AdError::Shape(format!("Grads::wrt: variable {} not on this tape", v.id))),
        }
    }
}

fn broadcast_len(a: usize, b: usize) -> Result<usize, AdError> {
    if a == b || b == 1 {
        Ok(a)
    } else if a == 1 {
        Ok(b)
    } else {
        Err(AdError::Shape(format!("cannot broadcast arrays of length {a} and {b}")))
    }
}

#[inline]
fn at(v: &[f64], i: usize) -> f64 {
    if v.len() == 1 { v[0] } else { v[i] }
}

impl Tape {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    #[must_use]
    pub fn stored_values(&self) -> usize {
        self.nodes
            .iter()
            .map(|n| {
                n.value.len()
                    + match &n.op {
                        Op::Elementwise { partials, .. } => partials.iter().map(Vec::len).sum(),
                        _ => 0,
                    }
            })
            .sum()
    }

    fn push(&mut self, value: Vec<f64>, op: Op) -> Var {
        let len = value.len();
        self.nodes.push(Node { value, op });
        Var { id: self.nodes.len() - 1, len }
    }

    fn check(&self, v: Var) -> Result<&[f64], AdError> {
        match self.nodes.get(v.id) {
            Some(n) if n.value.len() == v.len => Ok(&n.value),
            _ => Err(AdError::Shape(format!("variable {} is not on this tape", v.id))),
        }
    }



    pub fn value(&self, v: Var) -> Result<&[f64], AdError> {
        self.check(v)
    }

    pub fn input(&mut self, value: Vec<f64>) -> Var {
        self.push(value, Op::Leaf)
    }

    pub fn constant(&mut self, value: Vec<f64>) -> Var {
        self.push(value, Op::Constant)
    }



    pub fn stop_gradient(&mut self, x: Var) -> Result<Var, AdError> {
        let v = self.check(x)?.to_vec();
        Ok(self.constant(v))
    }

    fn binary(&mut self, a: Var, b: Var, f: impl Fn(f64, f64) -> f64) -> Result<(Vec<f64>, usize), AdError> {
        let (av, bv) = (self.check(a)?, self.check(b)?);
        let n = broadcast_len(av.len(), bv.len())?;
        Ok(((0..n).map(|i| f(at(av, i), at(bv, i))).collect(), n))
    }



    pub fn add(&mut self, a: Var, b: Var) -> Result<Var, AdError> {
        let (v, _) = self.binary(a, b, |x, y| x + y)?;
        Ok(self.push(v, Op::Add { a: a.id, b: b.id }))
    }



    pub fn sub(&mut self, a: Var, b: Var) -> Result<Var, AdError> {
        let (v, _) = self.binary(a, b, |x, y| x - y)?;
        Ok(self.push(v, Op::Sub { a: a.id, b: b.id }))
    }



    pub fn mul(&mut self, a: Var, b: Var) -> Result<Var, AdError> {
        let (v, _) = self.binary(a, b, |x, y| x * y)?;
        Ok(self.push(v, Op::Mul { a: a.id, b: b.id }))
    }



    pub fn div(&mut self, a: Var, b: Var) -> Result<Var, AdError> {
        let (v, _) = self.binary(a, b, |x, y| x / y)?;
        Ok(self.push(v, Op::Div { a: a.id, b: b.id }))
    }



    pub fn scale(&mut self, x: Var, c: f64) -> Result<Var, AdError> {
        let v = self.check(x)?.iter().map(|&e| c * e).collect();
        Ok(self.push(v, Op::Scale { x: x.id, c }))
    }



    pub fn neg(&mut self, x: Var) -> Result<Var, AdError> {
        self.scale(x, -1.0)
    }



    pub fn add_scalar(&mut self, x: Var, c: f64) -> Result<Var, AdError> {
        self.map(x, |d| d + c)
    }



    pub fn map(&mut self, x: Var, f: impl Fn(Dual<1>) -> Dual<1>) -> Result<Var, AdError> {
        let xv = self.check(x)?;
        let mut value = Vec::with_capacity(xv.len());
        let mut d = Vec::with_capacity(xv.len());
        for &e in xv {
            let y = f(Dual::variable(e, 0));
            value.push(y.re);
            d.push(y.eps[0]);
        }
        Ok(self.push(value, Op::Elementwise { args: vec![x.id], partials: vec![d] }))
    }



    pub fn map2(&mut self, a: Var, b: Var, f: impl Fn(Dual<2>, Dual<2>) -> Dual<2>) -> Result<Var, AdError> {
        let (av, bv) = (self.check(a)?, self.check(b)?);
        let n = broadcast_len(av.len(), bv.len())?;
        let mut value = Vec::with_capacity(n);
        let mut da = Vec::with_capacity(n);
        let mut db = Vec::with_capacity(n);
        for i in 0..n {
            let y = f(Dual::variable(at(av, i), 0), Dual::variable(at(bv, i), 1));
            value.push(y.re);
            da.push(y.eps[0]);
            db.push(y.eps[1]);
        }
        Ok(self.push(value, Op::Elementwise { args: vec![a.id, b.id], partials: vec![da, db] }))
    }



    pub fn exp(&mut self, x: Var) -> Result<Var, AdError> {
        self.map(x, Scalar::exp)
    }



    pub fn ln(&mut self, x: Var) -> Result<Var, AdError> {
        self.map(x, Scalar::ln)
    }



    pub fn sqrt(&mut self, x: Var) -> Result<Var, AdError> {
        self.map(x, Scalar::sqrt)
    }



    pub fn sin(&mut self, x: Var) -> Result<Var, AdError> {
        self.map(x, Scalar::sin)
    }



    pub fn cos(&mut self, x: Var) -> Result<Var, AdError> {
        self.map(x, Scalar::cos)
    }



    pub fn tanh(&mut self, x: Var) -> Result<Var, AdError> {
        self.map(x, Scalar::tanh)
    }



    pub fn square(&mut self, x: Var) -> Result<Var, AdError> {
        self.map(x, Scalar::square)
    }



    pub fn powf(&mut self, x: Var, y: f64) -> Result<Var, AdError> {
        self.map(x, |d| d.powf(y))
    }



    pub fn maximum(&mut self, a: Var, b: Var) -> Result<Var, AdError> {
        self.map2(a, b, Scalar::maximum)
    }



    pub fn minimum(&mut self, a: Var, b: Var) -> Result<Var, AdError> {
        self.map2(a, b, Scalar::minimum)
    }



    pub fn select(&mut self, mask: &[bool], a: Var, b: Var) -> Result<Var, AdError> {
        let (av, bv) = (self.check(a)?, self.check(b)?);
        let n = broadcast_len(av.len(), bv.len())?;
        if mask.len() != n {
            return Err(AdError::Shape(format!("select: mask of length {} for {n} entries", mask.len())));
        }
        let value: Vec<f64> = (0..n).map(|i| if mask[i] { at(av, i) } else { at(bv, i) }).collect();
        let da = mask.iter().map(|&m| if m { 1.0 } else { 0.0 }).collect();
        let db = mask.iter().map(|&m| if m { 0.0 } else { 1.0 }).collect();
        Ok(self.push(value, Op::Elementwise { args: vec![a.id, b.id], partials: vec![da, db] }))
    }



    pub fn sum(&mut self, x: Var) -> Result<Var, AdError> {
        let s = self.check(x)?.iter().sum();
        Ok(self.push(vec![s], Op::Sum { x: x.id }))
    }



    pub fn mean(&mut self, x: Var) -> Result<Var, AdError> {
        if x.len == 0 {
            return Err(AdError::Shape("mean of an empty array".into()));
        }
        let s = self.sum(x)?;
        self.scale(s, 1.0 / x.len as f64)
    }



    pub fn dot(&mut self, a: Var, b: Var) -> Result<Var, AdError> {
        let (av, bv) = (self.check(a)?, self.check(b)?);
        if av.len() != bv.len() {
            return Err(AdError::Shape(format!("dot: lengths {} and {}", av.len(), bv.len())));
        }
        let s = av.iter().zip(bv).map(|(x, y)| x * y).sum();
        Ok(self.push(vec![s], Op::Dot { a: a.id, b: b.id }))
    }



    pub fn gather(&mut self, x: Var, idx: Vec<usize>) -> Result<Var, AdError> {
        let xv = self.check(x)?;
        if let Some(&bad) = idx.iter().find(|&&i| i >= xv.len()) {
            return Err(AdError::Shape(format!("gather: index {bad} out of range {}", xv.len())));
        }
        let v = idx.iter().map(|&i| xv[i]).collect();
        Ok(self.push(v, Op::Gather { x: x.id, idx }))
    }



    pub fn scatter_add(&mut self, x: Var, idx: Vec<usize>, len: usize) -> Result<Var, AdError> {
        let xv = self.check(x)?;
        if idx.len() != xv.len() {
            return Err(AdError::Shape(format!(
                "scatter_add: {} indices for {} values",
                idx.len(),
                xv.len()
            )));
        }
        let mut v = vec![0.0; len];
        for (&i, &e) in idx.iter().zip(xv) {
            *v.get_mut(i)
                .ok_or_else(|| AdError::Shape(format!("scatter_add: index {i} out of range {len}")))? += e;
        }
        Ok(self.push(v, Op::ScatterAdd { x: x.id, idx }))
    }



    pub fn roll(&mut self, x: Var, shift: isize) -> Result<Var, AdError> {
        let n = x.len;
        if n == 0 {
            return self.gather(x, Vec::new());
        }
        let n_i = isize::try_from(n).map_err(|_| AdError::Shape("roll: array too long".into()))?;
        let s = shift.rem_euclid(n_i).unsigned_abs();
        let idx = (0..n).map(|i| (i + n - s) % n).collect();
        self.gather(x, idx)
    }



    pub fn concat(&mut self, parts: &[Var]) -> Result<Var, AdError> {
        let mut v = Vec::new();
        for &p in parts {
            v.extend_from_slice(self.check(p)?);
        }
        Ok(self.push(v, Op::Concat { parts: parts.iter().map(|p| p.id).collect() }))
    }



    pub fn slice(&mut self, x: Var, start: usize, len: usize) -> Result<Var, AdError> {
        let xv = self.check(x)?;
        let end = start.checked_add(len).filter(|&e| e <= xv.len()).ok_or_else(|| {
            AdError::Shape(format!("slice {start}..{start}+{len} out of range {}", xv.len()))
        })?;
        let v = xv[start..end].to_vec();
        Ok(self.push(v, Op::Slice { x: x.id, start }))
    }



    pub fn matvec(&mut self, m: Arc<Vec<f64>>, rows: usize, cols: usize, x: Var) -> Result<Var, AdError> {
        let xv = self.check(x)?;
        if m.len() != rows * cols || xv.len() != cols {
            return Err(AdError::Shape(format!(
                "matvec: matrix of {} entries as {rows}×{cols} times vector of {}",
                m.len(),
                xv.len()
            )));
        }
        let v =
            (0..rows).map(|i| m[i * cols..(i + 1) * cols].iter().zip(xv).map(|(a, b)| a * b).sum()).collect();
        Ok(self.push(v, Op::DenseMatVec { x: x.id, m, rows, cols }))
    }



    pub fn sparse_matvec(&mut self, m: Arc<SparseConst>, x: Var) -> Result<Var, AdError> {
        let xv = self.check(x)?;
        if xv.len() != m.ncols {
            return Err(AdError::Shape(format!(
                "sparse_matvec: {} columns, vector of {}",
                m.ncols,
                xv.len()
            )));
        }
        let v = (0..m.nrows)
            .map(|r| (m.indptr[r]..m.indptr[r + 1]).map(|k| m.data[k] * xv[m.indices[k]]).sum())
            .collect();
        Ok(self.push(v, Op::SparseMatVec { x: x.id, m }))
    }



    pub fn custom(&mut self, inputs: &[Var], value: Vec<f64>, pullback: Pullback) -> Result<Var, AdError> {
        for &i in inputs {
            self.check(i)?;
        }
        Ok(self.push(value, Op::Custom { inputs: inputs.iter().map(|v| v.id).collect(), pullback }))
    }



    pub fn vjp(&self, output: Var, cotangent: &[f64]) -> Result<Grads, AdError> {
        self.check(output)?;
        if cotangent.len() != output.len {
            return Err(AdError::Shape(format!(
                "vjp: cotangent of length {} for output of length {}",
                cotangent.len(),
                output.len
            )));
        }
        let lens: Vec<usize> = self.nodes.iter().map(|n| n.value.len()).collect();
        let mut grads: Vec<Option<Vec<f64>>> = vec![None; self.nodes.len()];
        grads[output.id] = Some(cotangent.to_vec());
        for id in (0..=output.id).rev() {
            let Some(g) = grads[id].take() else { continue };
            self.pull(id, &g, &mut grads)?;
            grads[id] = Some(g);
        }
        Ok(Grads { grads, lens })
    }



    pub fn grad(&self, output: Var) -> Result<Grads, AdError> {
        if output.len != 1 {
            return Err(AdError::Shape(format!("grad of a non-scalar output of length {}", output.len)));
        }
        self.vjp(output, &[1.0])
    }



    pub fn jacrev(&self, output: Var, wrt: &[Var]) -> Result<Vec<Vec<f64>>, AdError> {
        let m = output.len;
        let mut blocks: Vec<Vec<f64>> = wrt.iter().map(|v| vec![0.0; m * v.len]).collect();
        let mut seed = vec![0.0; m];
        for i in 0..m {
            seed[i] = 1.0;
            let g = self.vjp(output, &seed)?;
            for (block, &v) in blocks.iter_mut().zip(wrt) {
                block[i * v.len..(i + 1) * v.len].copy_from_slice(&g.wrt(v)?);
            }
            seed[i] = 0.0;
        }
        Ok(blocks)
    }

    #[allow(clippy::too_many_lines)]
    fn pull(&self, id: usize, g: &[f64], grads: &mut [Option<Vec<f64>>]) -> Result<(), AdError> {
        let val = |i: usize| self.nodes[i].value.as_slice();
        match &self.nodes[id].op {
            Op::Leaf | Op::Constant => {}
            Op::Scale { x, c } => {
                let c = *c;
                accumulate(grads, &self.nodes, *x, g.iter().map(|e| c * e).collect());
            }
            Op::Add { a, b } => {
                accumulate(grads, &self.nodes, *a, g.to_vec());
                accumulate(grads, &self.nodes, *b, g.to_vec());
            }
            Op::Sub { a, b } => {
                accumulate(grads, &self.nodes, *a, g.to_vec());
                accumulate(grads, &self.nodes, *b, g.iter().map(|e| -e).collect());
            }
            Op::Mul { a, b } => {
                let (av, bv) = (val(*a), val(*b));
                let ga = (0..g.len()).map(|i| g[i] * at(bv, i)).collect();
                let gb = (0..g.len()).map(|i| g[i] * at(av, i)).collect();
                accumulate(grads, &self.nodes, *a, ga);
                accumulate(grads, &self.nodes, *b, gb);
            }
            Op::Div { a, b } => {
                let (av, bv) = (val(*a), val(*b));
                let ga = (0..g.len()).map(|i| g[i] / at(bv, i)).collect();
                let gb = (0..g.len())
                    .map(|i| {
                        let y = at(bv, i);
                        -g[i] * at(av, i) / (y * y)
                    })
                    .collect();
                accumulate(grads, &self.nodes, *a, ga);
                accumulate(grads, &self.nodes, *b, gb);
            }
            Op::Elementwise { args, partials } => {
                for (&arg, d) in args.iter().zip(partials) {
                    let ga = g.iter().zip(d).map(|(e, p)| if *e == 0.0 { 0.0 } else { e * p }).collect();
                    accumulate(grads, &self.nodes, arg, ga);
                }
            }
            Op::Sum { x } => {
                accumulate(grads, &self.nodes, *x, vec![g[0]; val(*x).len()]);
            }
            Op::Dot { a, b } => {
                let (av, bv) = (val(*a), val(*b));
                accumulate(grads, &self.nodes, *a, bv.iter().map(|e| g[0] * e).collect());
                accumulate(grads, &self.nodes, *b, av.iter().map(|e| g[0] * e).collect());
            }
            Op::Gather { x, idx } => {
                let mut gx = vec![0.0; val(*x).len()];
                for (&i, &e) in idx.iter().zip(g) {
                    gx[i] += e;
                }
                accumulate(grads, &self.nodes, *x, gx);
            }
            Op::ScatterAdd { x, idx } => {
                accumulate(grads, &self.nodes, *x, idx.iter().map(|&i| g[i]).collect());
            }
            Op::Concat { parts } => {
                let mut off = 0;
                for &p in parts {
                    let n = val(p).len();
                    accumulate(grads, &self.nodes, p, g[off..off + n].to_vec());
                    off += n;
                }
            }
            Op::Slice { x, start } => {
                let mut gx = vec![0.0; val(*x).len()];
                gx[*start..*start + g.len()].copy_from_slice(g);
                accumulate(grads, &self.nodes, *x, gx);
            }
            Op::DenseMatVec { x, m, rows, cols } => {
                let mut gx = vec![0.0; *cols];
                for i in 0..*rows {
                    let gi = g[i];
                    for (gxj, mij) in gx.iter_mut().zip(&m[i * cols..(i + 1) * cols]) {
                        *gxj += mij * gi;
                    }
                }
                accumulate(grads, &self.nodes, *x, gx);
            }
            Op::SparseMatVec { x, m } => {
                let mut gx = vec![0.0; m.ncols];
                for (r, &gr) in g.iter().enumerate() {
                    for k in m.indptr[r]..m.indptr[r + 1] {
                        gx[m.indices[k]] += m.data[k] * gr;
                    }
                }
                accumulate(grads, &self.nodes, *x, gx);
            }
            Op::Custom { inputs, pullback } => {
                let cts = pullback(g)?;
                if cts.len() != inputs.len() {
                    return Err(AdError::Shape(format!(
                        "custom pullback returned {} cotangents for {} inputs",
                        cts.len(),
                        inputs.len()
                    )));
                }
                for (&i, ct) in inputs.iter().zip(cts) {
                    if ct.len() != val(i).len() {
                        return Err(AdError::Shape(format!(
                            "custom pullback cotangent of length {} for input of length {}",
                            ct.len(),
                            val(i).len()
                        )));
                    }
                    accumulate(grads, &self.nodes, i, ct);
                }
            }
        }
        Ok(())
    }
}

fn accumulate(grads: &mut [Option<Vec<f64>>], nodes: &[Node], target: usize, contribution: Vec<f64>) {
    if matches!(nodes[target].op, Op::Constant) {
        return;
    }
    let tlen = nodes[target].value.len();
    let contribution =
        if tlen == 1 && contribution.len() != 1 { vec![contribution.iter().sum()] } else { contribution };
    match &mut grads[target] {
        Some(acc) => {
            for (a, c) in acc.iter_mut().zip(&contribution) {
                *a += c;
            }
        }
        slot @ None => *slot = Some(contribution),
    }
}
