// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





use std::cell::RefCell;
use std::fmt::Debug;
use std::ops::{Add, Div, Mul, Neg, Sub};

use crate::error::{GResult, GeometryError};

pub trait Scalar:
    Copy
    + Debug
    + Send
    + Sync
    + 'static
    + Add<Output = Self>
    + Sub<Output = Self>
    + Mul<Output = Self>
    + Div<Output = Self>
    + Neg<Output = Self>
    + Add<f64, Output = Self>
    + Sub<f64, Output = Self>
    + Mul<f64, Output = Self>
    + Div<f64, Output = Self>
{
    fn cst(v: f64) -> Self;
    fn val(self) -> f64;
    fn chain(value: f64, parts: &[(Self, f64)]) -> Self;

    #[must_use]
    fn sqrt(self) -> Self {
        let v = self.val().sqrt();
        Self::chain(v, &[(self, 0.5 / v)])
    }
    #[must_use]
    fn exp(self) -> Self {
        let v = self.val().exp();
        Self::chain(v, &[(self, v)])
    }
    #[must_use]
    fn ln(self) -> Self {
        let x = self.val();
        Self::chain(x.ln(), &[(self, 1.0 / x)])
    }
    #[must_use]
    fn ln_1p(self) -> Self {
        let x = self.val();
        Self::chain(x.ln_1p(), &[(self, 1.0 / (1.0 + x))])
    }
    #[must_use]
    fn tanh(self) -> Self {
        let t = self.val().tanh();
        Self::chain(t, &[(self, (1.0 + t) * (1.0 - t))])
    }
    #[must_use]
    fn sin(self) -> Self {
        let x = self.val();
        Self::chain(x.sin(), &[(self, x.cos())])
    }
    #[must_use]
    fn cos(self) -> Self {
        let x = self.val();
        Self::chain(x.cos(), &[(self, -x.sin())])
    }
    #[must_use]
    fn atan2(self, x: Self) -> Self {
        let (yv, xv) = (self.val(), x.val());
        let r2 = xv * xv + yv * yv;
        Self::chain(yv.atan2(xv), &[(self, xv / r2), (x, -yv / r2)])
    }
    #[must_use]
    fn powi(self, n: i32) -> Self {
        let x = self.val();
        if n == 0 {
            return Self::cst(1.0);
        }
        Self::chain(x.powi(n), &[(self, f64::from(n) * x.powi(n - 1))])
    }
    #[must_use]
    fn powf(self, p: f64) -> Self {
        let x = self.val();
        Self::chain(x.powf(p), &[(self, p * x.powf(p - 1.0))])
    }
    #[must_use]
    fn abs(self) -> Self {
        let x = self.val();
        let s = if x >= 0.0 { 1.0 } else { -1.0 };
        Self::chain(x.abs(), &[(self, s)])
    }
    #[must_use]
    fn min(self, other: Self) -> Self {
        let (a, b) = (self.val(), other.val());
        if a < b {
            self
        } else if b < a {
            other
        } else if a.is_nan() || b.is_nan() {
            Self::chain(f64::NAN, &[(self, f64::NAN), (other, f64::NAN)])
        } else {
            Self::chain(a, &[(self, 0.5), (other, 0.5)])
        }
    }
    #[must_use]
    fn max(self, other: Self) -> Self {
        let (a, b) = (self.val(), other.val());
        if a > b {
            self
        } else if b > a {
            other
        } else if a.is_nan() || b.is_nan() {
            Self::chain(f64::NAN, &[(self, f64::NAN), (other, f64::NAN)])
        } else {
            Self::chain(a, &[(self, 0.5), (other, 0.5)])
        }
    }
    #[must_use]
    fn min_c(self, c: f64) -> Self {
        self.min(Self::cst(c))
    }
    #[must_use]
    fn max_c(self, c: f64) -> Self {
        self.max(Self::cst(c))
    }
    #[must_use]
    fn clip(self, lo: Self, hi: Self) -> Self {
        self.max(lo).min(hi)
    }
    #[must_use]
    fn clip_c(self, lo: f64, hi: f64) -> Self {
        self.max_c(lo).min_c(hi)
    }
    #[must_use]
    fn sigmoid(self) -> Self {
        let x = self.val();
        let s = logistic(x);
        Self::chain(s, &[(self, s * (1.0 - s))])
    }
    #[must_use]
    fn softplus(self) -> Self {
        let x = self.val();
        let v = x.max(0.0) + (-(x.abs())).exp().ln_1p();
        Self::chain(v, &[(self, logistic(x))])
    }
    #[must_use]
    fn relu(self) -> Self {
        let x = self.val();
        if x > 0.0 { self } else { Self::chain(0.0, &[(self, 0.0)]) }
    }
    #[must_use]
    fn round_even(self) -> Self {
        Self::cst(self.val().round_ties_even())
    }
    #[must_use]
    fn floor(self) -> Self {
        Self::cst(self.val().floor())
    }
}

#[must_use]
pub fn logistic(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

#[must_use]
pub fn reduce_max<S: Scalar>(xs: &[S]) -> S {
    reduce_choose(xs, |a, b| a > b)
}

#[must_use]
pub fn reduce_min<S: Scalar>(xs: &[S]) -> S {
    reduce_choose(xs, |a, b| a < b)
}

fn reduce_choose<S: Scalar>(xs: &[S], better: impl Fn(f64, f64) -> bool) -> S {
    let Some(first) = xs.first() else {
        return S::cst(f64::NAN);
    };
    let mut best = first.val();
    for x in &xs[1..] {
        let v = x.val();
        if better(v, best) || v.is_nan() {
            best = v;
        }
    }
    if best.is_nan() {
        let parts: Vec<(S, f64)> = xs.iter().map(|x| (*x, f64::NAN)).collect();
        return S::chain(f64::NAN, &parts);
    }
    let count = xs.iter().filter(|x| x.val() == best).count();
    if count == 1
        && let Some(x) = xs.iter().find(|x| x.val() == best)
    {
        return *x;
    }
    #[allow(clippy::cast_precision_loss)]
    let w = 1.0 / count as f64;
    let parts: Vec<(S, f64)> = xs.iter().filter(|x| x.val() == best).map(|x| (*x, w)).collect();
    S::chain(best, &parts)
}

impl Scalar for f64 {
    #[inline]
    fn cst(v: f64) -> Self {
        v
    }
    #[inline]
    fn val(self) -> f64 {
        self
    }
    #[inline]
    fn chain(value: f64, _parts: &[(Self, f64)]) -> Self {
        value
    }
    #[inline]
    fn sqrt(self) -> Self {
        f64::sqrt(self)
    }
    #[inline]
    fn exp(self) -> Self {
        f64::exp(self)
    }
    #[inline]
    fn ln(self) -> Self {
        f64::ln(self)
    }
    #[inline]
    fn ln_1p(self) -> Self {
        f64::ln_1p(self)
    }
    #[inline]
    fn tanh(self) -> Self {
        f64::tanh(self)
    }
    #[inline]
    fn sin(self) -> Self {
        f64::sin(self)
    }
    #[inline]
    fn cos(self) -> Self {
        f64::cos(self)
    }
    #[inline]
    fn atan2(self, x: Self) -> Self {
        f64::atan2(self, x)
    }
    #[inline]
    fn powi(self, n: i32) -> Self {
        f64::powi(self, n)
    }
    #[inline]
    fn powf(self, p: f64) -> Self {
        f64::powf(self, p)
    }
    #[inline]
    fn abs(self) -> Self {
        f64::abs(self)
    }
    #[inline]
    fn min(self, other: Self) -> Self {
        if self.is_nan() || other.is_nan() { f64::NAN } else { f64::min(self, other) }
    }
    #[inline]
    fn max(self, other: Self) -> Self {
        if self.is_nan() || other.is_nan() { f64::NAN } else { f64::max(self, other) }
    }
    #[inline]
    fn sigmoid(self) -> Self {
        logistic(self)
    }
    #[inline]
    fn relu(self) -> Self {
        if self > 0.0 { self } else { 0.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dual<const N: usize> {
    pub v: f64,
    pub d: [f64; N],
}

impl<const N: usize> Dual<N> {
    #[must_use]
    pub fn var(v: f64, i: usize) -> Self {
        let mut d = [0.0; N];
        if i < N {
            d[i] = 1.0;
        }
        Self { v, d }
    }
}

impl<const N: usize> Scalar for Dual<N> {
    #[inline]
    fn cst(v: f64) -> Self {
        Self { v, d: [0.0; N] }
    }
    #[inline]
    fn val(self) -> f64 {
        self.v
    }
    #[inline]
    fn chain(value: f64, parts: &[(Self, f64)]) -> Self {
        let mut d = [0.0; N];
        for (x, w) in parts {
            for (di, xi) in d.iter_mut().zip(x.d.iter()) {
                *di += w * xi;
            }
        }
        Self { v: value, d }
    }
}

impl<const N: usize> Add for Dual<N> {
    type Output = Self;
    #[inline]
    fn add(self, o: Self) -> Self {
        let mut d = self.d;
        for (a, b) in d.iter_mut().zip(o.d.iter()) {
            *a += b;
        }
        Self { v: self.v + o.v, d }
    }
}
impl<const N: usize> Sub for Dual<N> {
    type Output = Self;
    #[inline]
    fn sub(self, o: Self) -> Self {
        let mut d = self.d;
        for (a, b) in d.iter_mut().zip(o.d.iter()) {
            *a -= b;
        }
        Self { v: self.v - o.v, d }
    }
}
impl<const N: usize> Mul for Dual<N> {
    type Output = Self;
    #[inline]
    fn mul(self, o: Self) -> Self {
        let mut d = [0.0; N];
        for i in 0..N {
            d[i] = self.d[i] * o.v + self.v * o.d[i];
        }
        Self { v: self.v * o.v, d }
    }
}
impl<const N: usize> Div for Dual<N> {
    type Output = Self;
    #[inline]
    fn div(self, o: Self) -> Self {
        let q = self.v / o.v;
        let mut d = [0.0; N];
        for i in 0..N {
            d[i] = self.d[i] / o.v - q * o.d[i] / o.v;
        }
        Self { v: q, d }
    }
}
impl<const N: usize> Neg for Dual<N> {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        let mut d = self.d;
        for a in &mut d {
            *a = -*a;
        }
        Self { v: -self.v, d }
    }
}
impl<const N: usize> Add<f64> for Dual<N> {
    type Output = Self;
    #[inline]
    fn add(self, o: f64) -> Self {
        Self { v: self.v + o, d: self.d }
    }
}
impl<const N: usize> Sub<f64> for Dual<N> {
    type Output = Self;
    #[inline]
    fn sub(self, o: f64) -> Self {
        Self { v: self.v - o, d: self.d }
    }
}
impl<const N: usize> Mul<f64> for Dual<N> {
    type Output = Self;
    #[inline]
    fn mul(self, o: f64) -> Self {
        let mut d = self.d;
        for a in &mut d {
            *a *= o;
        }
        Self { v: self.v * o, d }
    }
}
impl<const N: usize> Div<f64> for Dual<N> {
    type Output = Self;
    #[inline]
    fn div(self, o: f64) -> Self {
        let mut d = self.d;
        for a in &mut d {
            *a /= o;
        }
        Self { v: self.v / o, d }
    }
}

const NONE: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct Entry {
    a: u32,
    b: u32,
    da: f64,
    db: f64,
}

struct TapeInner {
    entries: Vec<Entry>,
}

thread_local! {
    static TAPE: RefCell<Option<TapeInner>> = const { RefCell::new(None) };
}

fn push(entry: Entry) -> u32 {
    TAPE.with(|t| {
        let mut t = t.borrow_mut();
        match t.as_mut() {
            Some(tape) => {
                let i = u32::try_from(tape.entries.len()).unwrap_or(NONE);
                tape.entries.push(entry);
                i
            }

            None => NONE,
        }
    })
}


#[derive(Clone, Copy, Debug)]
pub struct Rv {
    idx: u32,
    v: f64,
}

impl Rv {
    #[must_use]
    pub fn index(self) -> Option<usize> {
        (self.idx != NONE).then_some(self.idx as usize)
    }

    fn unary(v: f64, a: Self, da: f64) -> Self {
        if a.idx == NONE {
            return Self { idx: NONE, v };
        }
        Self { idx: push(Entry { a: a.idx, b: NONE, da, db: 0.0 }), v }
    }

    fn binary(v: f64, a: Self, da: f64, b: Self, db: f64) -> Self {
        match (a.idx == NONE, b.idx == NONE) {
            (true, true) => Self { idx: NONE, v },
            (false, true) => Self::unary(v, a, da),
            (true, false) => Self::unary(v, b, db),
            (false, false) => Self { idx: push(Entry { a: a.idx, b: b.idx, da, db }), v },
        }
    }
}

impl Scalar for Rv {
    #[inline]
    fn cst(v: f64) -> Self {
        Self { idx: NONE, v }
    }
    #[inline]
    fn val(self) -> f64 {
        self.v
    }
    fn chain(value: f64, parts: &[(Self, f64)]) -> Self {
        let live: Vec<&(Self, f64)> = parts.iter().filter(|(x, _)| x.idx != NONE).collect();
        match live.len() {
            0 => Self::cst(value),
            1 => Self::unary(value, live[0].0, live[0].1),
            _ => {
                let mut acc = Self::binary(value, live[0].0, live[0].1, live[1].0, live[1].1);
                for (x, w) in &live[2..] {
                    acc = Self::binary(value, acc, 1.0, *x, *w);
                }
                acc
            }
        }
    }
}

impl Add for Rv {
    type Output = Self;
    #[inline]
    fn add(self, o: Self) -> Self {
        Self::binary(self.v + o.v, self, 1.0, o, 1.0)
    }
}
impl Sub for Rv {
    type Output = Self;
    #[inline]
    fn sub(self, o: Self) -> Self {
        Self::binary(self.v - o.v, self, 1.0, o, -1.0)
    }
}
impl Mul for Rv {
    type Output = Self;
    #[inline]
    fn mul(self, o: Self) -> Self {
        Self::binary(self.v * o.v, self, o.v, o, self.v)
    }
}
impl Div for Rv {
    type Output = Self;
    #[inline]
    fn div(self, o: Self) -> Self {
        let q = self.v / o.v;
        Self::binary(q, self, 1.0 / o.v, o, -q / o.v)
    }
}
impl Neg for Rv {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Self::unary(-self.v, self, -1.0)
    }
}
impl Add<f64> for Rv {
    type Output = Self;
    #[inline]
    fn add(self, o: f64) -> Self {
        Self::unary(self.v + o, self, 1.0)
    }
}
impl Sub<f64> for Rv {
    type Output = Self;
    #[inline]
    fn sub(self, o: f64) -> Self {
        Self::unary(self.v - o, self, 1.0)
    }
}
impl Mul<f64> for Rv {
    type Output = Self;
    #[inline]
    fn mul(self, o: f64) -> Self {
        Self::unary(self.v * o, self, o)
    }
}
impl Div<f64> for Rv {
    type Output = Self;
    #[inline]
    fn div(self, o: f64) -> Self {
        Self::unary(self.v / o, self, 1.0 / o)
    }
}

pub struct RevTape {
    adj: Vec<f64>,
}

impl RevTape {

    pub fn begin() -> GResult<Self> {
        TAPE.with(|t| {
            let mut t = t.borrow_mut();
            if t.is_some() {
                return Err(GeometryError::Value(
                    "a reverse-mode tape is already active on this thread".into(),
                ));
            }
            *t = Some(TapeInner { entries: Vec::new() });
            Ok(Self { adj: Vec::new() })
        })
    }

    #[must_use]
    pub fn leaf(&self, v: f64) -> Rv {
        Rv { idx: push(Entry { a: NONE, b: NONE, da: 0.0, db: 0.0 }), v }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        TAPE.with(|t| t.borrow().as_ref().map_or(0, |tape| tape.entries.len()))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn sweep_to(&mut self, out: Rv, seed: f64, mark: usize) {
        TAPE.with(|t| {
            let mut t = t.borrow_mut();
            let Some(tape) = t.as_mut() else { return };
            let n = tape.entries.len();
            if self.adj.len() < n {
                self.adj.resize(n, 0.0);
            }
            if out.idx != NONE {
                self.adj[out.idx as usize] += seed;
            }
            for i in (mark..n).rev() {
                let g = self.adj[i];
                if g == 0.0 {
                    continue;
                }
                let e = tape.entries[i];
                if e.a != NONE {
                    self.adj[e.a as usize] += g * e.da;
                }
                if e.b != NONE {
                    self.adj[e.b as usize] += g * e.db;
                }
            }
            for a in &mut self.adj[mark..n] {
                *a = 0.0;
            }
            tape.entries.truncate(mark);
        });
    }

    pub fn finish(&mut self) {
        TAPE.with(|t| {
            let t = t.borrow();
            let Some(tape) = t.as_ref() else { return };
            let n = tape.entries.len();
            if self.adj.len() < n {
                self.adj.resize(n, 0.0);
            }
            for i in (0..n).rev() {
                let g = self.adj[i];
                if g == 0.0 {
                    continue;
                }
                let e = tape.entries[i];
                if e.a != NONE {
                    self.adj[e.a as usize] += g * e.da;
                }
                if e.b != NONE {
                    self.adj[e.b as usize] += g * e.db;
                }
            }
        });
    }

    #[must_use]
    pub fn adjoint(&self, x: Rv) -> f64 {
        x.index().and_then(|i| self.adj.get(i).copied()).unwrap_or(0.0)
    }
}

impl Drop for RevTape {
    fn drop(&mut self) {
        TAPE.with(|t| {
            *t.borrow_mut() = None;
        });
    }
}

