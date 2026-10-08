// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::cell::RefCell;
use std::ops::{Add, Div, Mul, Neg, Sub};
use std::sync::Arc;

use implexity_ad::{AdError, Dual, Scalar, Tape, Var};
use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Shape {
    dims: [usize; 4],
    ndim: u8,
}

impl Shape {
    #[must_use]
    pub fn new(dims: &[usize]) -> Self {
        let mut d = [1usize; 4];
        let n = dims.len().min(4);
        d[..n].copy_from_slice(&dims[..n]);
        #[allow(clippy::cast_possible_truncation)]
        Self { dims: d, ndim: n as u8 }
    }

    #[must_use]
    pub const fn scalar() -> Self {
        Self { dims: [1; 4], ndim: 0 }
    }

    #[must_use]
    pub const fn d3(s: [usize; 3]) -> Self {
        Self { dims: [s[0], s[1], s[2], 1], ndim: 3 }
    }

    #[must_use]
    pub const fn d1(n: usize) -> Self {
        Self { dims: [n, 1, 1, 1], ndim: 1 }
    }

    #[must_use]
    pub fn dims(&self) -> &[usize] {
        &self.dims[..self.ndim as usize]
    }

    #[must_use]
    pub fn ndim(&self) -> usize {
        self.ndim as usize
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.dims().iter().product()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    pub fn as3(&self) -> [usize; 3] {
        [self.dims[0], self.dims[1], self.dims[2]]
    }
}

#[derive(Clone, Copy)]
pub struct A<'g> {
    g: &'g Graph,
    v: Var,
    shape: Shape,
}

impl std::fmt::Debug for A<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "A(#{}, {:?})", self.v.index(), self.shape.dims())
    }
}

pub type Pullback = Box<dyn Fn(&[f64]) -> Result<Vec<Vec<f64>>, AdError> + Send + Sync>;

#[derive(Default)]
pub struct Graph {
    tape: RefCell<Tape>,
    err: RefCell<Option<String>>,
}

impl std::fmt::Debug for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Graph({} nodes)", self.tape.borrow().len())
    }
}

const PAR_MIN: usize = 4096;

fn bcast(a: Shape, b: Shape) -> Option<Shape> {
    if a == b || b.len() == 1 {
        Some(a)
    } else if a.len() == 1 {
        Some(b)
    } else {
        None
    }
}

impl Graph {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tape.borrow().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tape.borrow().is_empty()
    }

    fn record_err(&self, msg: impl Into<String>) {
        let mut e = self.err.borrow_mut();
        if e.is_none() {
            *e = Some(msg.into());
        }
    }

    pub fn check(&self) -> Result<(), String> {
        match self.err.borrow().as_ref() {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    fn wrap(&self, r: Result<Var, AdError>, shape: Shape) -> A<'_> {
        match r {
            Ok(v) => A { g: self, v, shape },
            Err(e) => self.failed(&e.to_string(), shape),
        }
    }

    pub fn failed(&self, msg: &str, shape: Shape) -> A<'_> {
        self.record_err(msg);
        let v = self.tape.borrow_mut().constant(vec![f64::NAN; shape.len()]);
        A { g: self, v, shape }
    }

    pub fn input(&self, value: Vec<f64>, shape: Shape) -> A<'_> {
        if value.len() != shape.len() {
            return self
                .failed(&format!("input of {} values for shape {:?}", value.len(), shape.dims()), shape);
        }
        let v = self.tape.borrow_mut().input(value);
        A { g: self, v, shape }
    }

    pub fn constant(&self, value: Vec<f64>, shape: Shape) -> A<'_> {
        if value.len() != shape.len() {
            return self
                .failed(&format!("constant of {} values for shape {:?}", value.len(), shape.dims()), shape);
        }
        let v = self.tape.borrow_mut().constant(value);
        A { g: self, v, shape }
    }

    pub fn scalar(&self, c: f64) -> A<'_> {
        self.constant(vec![c], Shape::scalar())
    }

    pub fn full(&self, c: f64, shape: Shape) -> A<'_> {
        self.constant(vec![c; shape.len()], shape)
    }

    #[must_use]
    pub fn value(&self, a: A<'_>) -> Vec<f64> {
        match self.tape.borrow().value(a.v) {
            Ok(v) => v.to_vec(),
            Err(e) => {
                self.record_err(e.to_string());
                vec![f64::NAN; a.shape.len()]
            }
        }
    }

    pub fn with_value<R>(&self, a: A<'_>, f: impl FnOnce(&[f64]) -> R) -> R {
        let t = self.tape.borrow();
        match t.value(a.v) {
            Ok(v) => f(v),
            Err(e) => {
                drop(t);
                self.record_err(e.to_string());
                f(&vec![f64::NAN; a.shape.len()])
            }
        }
    }

    pub fn custom<'g>(
        &'g self,
        inputs: &[A<'g>],
        value: Vec<f64>,
        shape: Shape,
        pullback: Pullback,
    ) -> A<'g> {
        if value.len() != shape.len() {
            return self.failed(
                &format!("custom node of {} values for shape {:?}", value.len(), shape.dims()),
                shape,
            );
        }
        let vars: Vec<Var> = inputs.iter().map(|a| a.v).collect();
        let r = self.tape.borrow_mut().custom(&vars, value, pullback);
        self.wrap(r, shape)
    }

    pub fn linear<'g>(
        &'g self,
        x: A<'g>,
        shape: Shape,
        forward: impl Fn(&[f64]) -> Vec<f64>,
        transpose: impl Fn(&[f64]) -> Vec<f64> + Send + Sync + 'static,
    ) -> A<'g> {
        let y = self.with_value(x, |v| forward(v));
        let nx = x.shape.len();
        self.custom(
            &[x],
            y,
            shape,
            Box::new(move |g: &[f64]| {
                let t = transpose(g);
                if t.len() == nx {
                    Ok(vec![t])
                } else {
                    Err(AdError::Shape(format!("linear transpose returned {} values for {nx}", t.len())))
                }
            }),
        )
    }

    pub fn mapn<'g, const N: usize>(
        &'g self,
        xs: [A<'g>; N],
        f: impl Fn([Dual<N>; N]) -> Dual<N> + Sync,
    ) -> A<'g> {
        let mut shape = xs[0].shape;
        for x in &xs[1..] {
            match bcast(shape, x.shape) {
                Some(s) => shape = s,
                None => {
                    return self.failed(
                        &format!("mapn: shapes {:?} and {:?} do not broadcast", shape.dims(), x.shape.dims()),
                        shape,
                    );
                }
            }
        }
        let n = shape.len();
        let vals: Vec<Vec<f64>> = xs.iter().map(|x| self.value(*x)).collect();
        let lens: Vec<usize> = vals.iter().map(Vec::len).collect();
        let eval = |i: usize| -> ([f64; N], f64) {
            let args: [Dual<N>; N] = std::array::from_fn(|k| {
                let v = if lens[k] == 1 { vals[k][0] } else { vals[k][i] };
                Dual::variable(v, k)
            });
            let y = f(args);
            (y.eps, y.re)
        };
        let res: Vec<([f64; N], f64)> = if n >= PAR_MIN {
            (0..n).into_par_iter().map(eval).collect()
        } else {
            (0..n).map(eval).collect()
        };
        let value: Vec<f64> = res.iter().map(|r| r.1).collect();
        let partials: Arc<Vec<[f64; N]>> = Arc::new(res.into_iter().map(|r| r.0).collect());
        let lens2 = lens.clone();
        self.custom(
            &xs,
            value,
            shape,
            Box::new(move |g: &[f64]| {
                let mut out = Vec::with_capacity(N);
                for (k, &len) in lens2.iter().enumerate() {
                    if len == g.len() {
                        out.push(g.iter().zip(partials.iter()).map(|(gi, p)| gi * p[k]).collect());
                    } else {
                        let s: f64 = g.iter().zip(partials.iter()).map(|(gi, p)| gi * p[k]).sum();
                        out.push(vec![s]);
                    }
                }
                Ok(out)
            }),
        )
    }

    pub fn mapn_multi<'g, const N: usize, const M: usize>(
        &'g self,
        xs: [A<'g>; N],
        f: impl Fn([Dual<N>; N]) -> [Dual<N>; M] + Sync,
    ) -> [A<'g>; M] {
        let mut shape = xs[0].shape;
        for x in &xs[1..] {
            if let Some(s) = bcast(shape, x.shape) {
                shape = s;
            } else {
                let e = self.failed("mapn_multi: shapes do not broadcast", shape);
                return [e; M];
            }
        }
        let n = shape.len();
        let vals: Vec<Vec<f64>> = xs.iter().map(|x| self.value(*x)).collect();
        let lens: Vec<usize> = vals.iter().map(Vec::len).collect();
        let eval = |i: usize| -> [Dual<N>; M] {
            let args: [Dual<N>; N] = std::array::from_fn(|k| {
                let v = if lens[k] == 1 { vals[k][0] } else { vals[k][i] };
                Dual::variable(v, k)
            });
            f(args)
        };
        let res: Vec<[Dual<N>; M]> = if n >= PAR_MIN {
            (0..n).into_par_iter().map(eval).collect()
        } else {
            (0..n).map(eval).collect()
        };
        std::array::from_fn(|m| {
            let value: Vec<f64> = res.iter().map(|r| r[m].re).collect();
            let partials: Arc<Vec<[f64; N]>> = Arc::new(res.iter().map(|r| r[m].eps).collect());
            let lens2 = lens.clone();
            self.custom(
                &xs,
                value,
                shape,
                Box::new(move |g: &[f64]| {
                    let mut out = Vec::with_capacity(N);
                    for (k, &len) in lens2.iter().enumerate() {
                        if len == g.len() {
                            out.push(g.iter().zip(partials.iter()).map(|(gi, p)| gi * p[k]).collect());
                        } else {
                            let s: f64 = g.iter().zip(partials.iter()).map(|(gi, p)| gi * p[k]).sum();
                            out.push(vec![s]);
                        }
                    }
                    Ok(out)
                }),
            )
        })
    }

    pub fn select<'g>(&'g self, mask: &[bool], a: A<'g>, b: A<'g>) -> A<'g> {
        let Some(shape) = bcast(a.shape, b.shape) else {
            return self.failed("select: shapes do not broadcast", a.shape);
        };
        let r = self.tape.borrow_mut().select(mask, a.v, b.v);
        self.wrap(r, shape)
    }

    pub fn stack<'g>(&'g self, parts: &[A<'g>]) -> A<'g> {
        if parts.is_empty() {
            return self.failed("stack of nothing", Shape::d1(0));
        }
        let s0 = parts[0].shape;
        if parts.iter().any(|p| p.shape != s0) {
            return self.failed("stack: unequal shapes", s0);
        }
        let mut dims = vec![parts.len()];
        dims.extend_from_slice(s0.dims());
        let vars: Vec<Var> = parts.iter().map(|p| p.v).collect();
        let r = self.tape.borrow_mut().concat(&vars);
        self.wrap(r, Shape::new(&dims))
    }

    pub fn concat<'g>(&'g self, parts: &[A<'g>], shape: Shape) -> A<'g> {
        let vars: Vec<Var> = parts.iter().map(|p| p.v).collect();
        let n: usize = parts.iter().map(|p| p.shape.len()).sum();
        if n != shape.len() {
            return self.failed("concat: lengths do not match the shape", shape);
        }
        let r = self.tape.borrow_mut().concat(&vars);
        self.wrap(r, shape)
    }

    pub fn grad(&self, out: A<'_>, wrt: &[A<'_>]) -> Result<Vec<Vec<f64>>, String> {
        self.check()?;
        let t = self.tape.borrow();
        let g = t.grad(out.v).map_err(|e| e.to_string())?;
        wrt.iter().map(|w| g.wrt(w.v).map_err(|e| e.to_string())).collect()
    }

    pub fn vjp(&self, out: A<'_>, ct: &[f64], wrt: &[A<'_>]) -> Result<Vec<Vec<f64>>, String> {
        self.check()?;
        let t = self.tape.borrow();
        let g = t.vjp(out.v, ct).map_err(|e| e.to_string())?;
        wrt.iter().map(|w| g.wrt(w.v).map_err(|e| e.to_string())).collect()
    }
}

impl<'g> A<'g> {
    #[must_use]
    pub fn graph(&self) -> &'g Graph {
        self.g
    }

    #[must_use]
    pub fn shape(&self) -> Shape {
        self.shape
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.shape.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shape.is_empty()
    }

    #[must_use]
    pub fn val(&self) -> Vec<f64> {
        self.g.value(*self)
    }

    #[must_use]
    pub fn item(&self) -> f64 {
        self.g.with_value(*self, |v| v.first().copied().unwrap_or(f64::NAN))
    }

    #[must_use]
    pub fn reshape(self, shape: Shape) -> Self {
        if shape.len() == self.shape.len() {
            Self { shape, ..self }
        } else {
            self.g.failed(&format!("reshape {:?} -> {:?}", self.shape.dims(), shape.dims()), shape)
        }
    }

    #[must_use]
    pub fn flat(self) -> Self {
        let n = self.len();
        self.reshape(Shape::d1(n))
    }

    #[must_use]
    pub fn map(self, f: impl Fn(Dual<1>) -> Dual<1> + Sync) -> Self {
        self.g.mapn([self], |[x]| f(x))
    }

    #[must_use]
    pub fn stop(self) -> Self {
        let r = self.g.tape.borrow_mut().stop_gradient(self.v);
        self.g.wrap(r, self.shape)
    }

    #[must_use]
    pub fn sum(self) -> Self {
        let r = self.g.tape.borrow_mut().sum(self.v);
        self.g.wrap(r, Shape::scalar())
    }

    #[must_use]
    pub fn mean(self) -> Self {
        let r = self.g.tape.borrow_mut().mean(self.v);
        self.g.wrap(r, Shape::scalar())
    }

    #[must_use]
    pub fn dot(self, b: Self) -> Self {
        let r = self.g.tape.borrow_mut().dot(self.v, b.v);
        self.g.wrap(r, Shape::scalar())
    }

    fn chooser(self, pick_max: bool) -> Self {
        let v = self.val();
        let best = if pick_max {
            v.iter()
                .copied()
                .fold(f64::NEG_INFINITY, |m, x| if x.is_nan() || m.is_nan() { f64::NAN } else { m.max(x) })
        } else {
            v.iter()
                .copied()
                .fold(f64::INFINITY, |m, x| if x.is_nan() || m.is_nan() { f64::NAN } else { m.min(x) })
        };
        #[allow(clippy::float_cmp)]                                
        let ind: Vec<f64> = v.iter().map(|&x| if x == best { 1.0 } else { 0.0 }).collect();
        let cnt: f64 = ind.iter().sum();
        let ind = Arc::new(ind);
        self.g.custom(
            &[self],
            vec![best],
            Shape::scalar(),
            Box::new(move |g: &[f64]| {
                let s = if cnt > 0.0 { g[0] / cnt } else { 0.0 };
                Ok(vec![ind.iter().map(|i| i * s).collect()])
            }),
        )
    }

    #[must_use]
    pub fn max(self) -> Self {
        self.chooser(true)
    }

    #[must_use]
    pub fn min(self) -> Self {
        self.chooser(false)
    }

    #[must_use]
    pub fn gather(self, idx: Vec<usize>, shape: Shape) -> Self {
        if idx.len() != shape.len() {
            return self.g.failed("gather: index count does not match the shape", shape);
        }
        let r = self.g.tape.borrow_mut().gather(self.v, idx);
        self.g.wrap(r, shape)
    }

    #[must_use]
    pub fn scatter_add(self, idx: Vec<usize>, shape: Shape) -> Self {
        let r = self.g.tape.borrow_mut().scatter_add(self.v, idx, shape.len());
        self.g.wrap(r, shape)
    }

    #[must_use]
    pub fn broadcast(self, shape: Shape) -> Self {
        if self.shape == shape {
            return self;
        }
        if self.len() != 1 {
            return self.g.failed("broadcast of a non-scalar", shape);
        }
        self.gather(vec![0; shape.len()], shape)
    }

    #[must_use]
    pub fn powf(self, p: f64) -> Self {
        self.map(move |x| x.powf(p))
    }

    #[must_use]
    pub fn exp(self) -> Self {
        self.map(Scalar::exp)
    }

    #[must_use]
    pub fn ln(self) -> Self {
        self.map(Scalar::ln)
    }

    #[must_use]
    pub fn sqrt(self) -> Self {
        self.map(Scalar::sqrt)
    }

    #[must_use]
    pub fn sigmoid(self) -> Self {
        self.map(Scalar::sigmoid)
    }

    #[must_use]
    pub fn softplus(self) -> Self {
        self.map(Scalar::softplus)
    }

    #[must_use]
    pub fn max_c(self, c: f64) -> Self {
        self.map(move |x| x.max_f64(c))
    }

    #[must_use]
    pub fn min_c(self, c: f64) -> Self {
        self.map(move |x| x.min_f64(c))
    }

    #[must_use]
    pub fn clip(self, lo: f64, hi: f64) -> Self {
        self.map(move |x| x.clip(lo, hi))
    }

    #[must_use]
    pub fn maximum(self, b: Self) -> Self {
        self.g.mapn([self, b], |[x, y]| x.maximum(y))
    }

    #[must_use]
    pub fn minimum(self, b: Self) -> Self {
        self.g.mapn([self, b], |[x, y]| x.minimum(y))
    }
}

macro_rules! binop {
    ($tr:ident, $m:ident, $tape:ident, $sc:expr, $rsc:expr) => {
        impl<'g> $tr for A<'g> {
            type Output = A<'g>;
            fn $m(self, b: A<'g>) -> A<'g> {
                let g = self.g;
                let Some(shape) = bcast(self.shape, b.shape) else {
                    return g.failed(
                        &format!(
                            "{}: shapes {:?} and {:?} do not broadcast",
                            stringify!($m),
                            self.shape.dims(),
                            b.shape.dims()
                        ),
                        self.shape,
                    );
                };
                let r = g.tape.borrow_mut().$tape(self.v, b.v);
                g.wrap(r, shape)
            }
        }
        impl<'g> $tr<f64> for A<'g> {
            type Output = A<'g>;
            fn $m(self, c: f64) -> A<'g> {
                let f: fn(A<'g>, f64) -> A<'g> = $sc;
                f(self, c)
            }
        }
        impl<'g> $tr<A<'g>> for f64 {
            type Output = A<'g>;
            fn $m(self, a: A<'g>) -> A<'g> {
                let f: fn(f64, A<'g>) -> A<'g> = $rsc;
                f(self, a)
            }
        }
    };
}

fn add_c(a: A<'_>, c: f64) -> A<'_> {
    let r = a.g.tape.borrow_mut().add_scalar(a.v, c);
    a.g.wrap(r, a.shape)
}

fn scale_c(a: A<'_>, c: f64) -> A<'_> {
    let r = a.g.tape.borrow_mut().scale(a.v, c);
    a.g.wrap(r, a.shape)
}

binop!(Add, add, add, |a, c| add_c(a, c), |c, a| add_c(a, c));
binop!(Sub, sub, sub, |a, c| add_c(a, -c), |c, a| a.map(move |x| -x + c));
binop!(Mul, mul, mul, |a, c| scale_c(a, c), |c, a| scale_c(a, c));
binop!(Div, div, div, |a, c| a.map(move |x| x / c), |c, a| a.map(move |x| Dual::constant(c) / x));

impl<'g> Neg for A<'g> {
    type Output = A<'g>;
    fn neg(self) -> A<'g> {
        let r = self.g.tape.borrow_mut().neg(self.v);
        self.g.wrap(r, self.shape)
    }
}

#[must_use]
pub fn relu<S: Scalar>(x: S) -> S {
    if x.value() > 0.0 { x } else { S::zero() }
}

#[must_use]
pub fn interp<S: Scalar>(x: S, xp: &[f64], fp: &[f64]) -> S {
    let n = xp.len();
    let xv = x.value();
    if n < 2 {
        return S::from_f64(fp.first().copied().unwrap_or(f64::NAN));
    }
    if xv < xp[0] {
        return S::from_f64(fp[0]);
    }
    if xv > xp[n - 1] {
        return S::from_f64(fp[n - 1]);
    }
    let mut i = xp.partition_point(|&t| t <= xv);
    i = i.clamp(1, n - 1);
    let df = fp[i] - fp[i - 1];
    let dx = xp[i] - xp[i - 1];
    let eps_spacing = f64::EPSILON * f64::EPSILON;
    if dx.abs() <= eps_spacing {
        return S::from_f64(fp[i - 1]);
    }
    let delta = x - xp[i - 1];
    delta / dx * df + fp[i - 1]
}

#[must_use]
pub fn fsum(v: &[f64]) -> f64 {
    v.iter().sum()
}

#[must_use]
pub fn idx3(s: [usize; 3], i: usize, j: usize, k: usize) -> usize {
    (i * s[1] + j) * s[2] + k
}

impl<'g> A<'g> {
    #[must_use]
    pub fn g_map2(
        self,
        b: A<'g>,
        f: impl Fn(Dual<2>, Dual<2>) -> Dual<2> + Sync,
    ) -> A<'g> {
        self.graph().mapn([self, b], |[x, y]| f(x, y))
    }
}
