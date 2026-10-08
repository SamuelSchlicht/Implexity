// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use core::fmt;
use core::iter::Sum;
use core::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

use crate::scalar::{Scalar, check_lift_shapes};

#[inline]
fn mulz(c: f64, t: f64) -> f64 {
    if t == 0.0 { 0.0 } else { c * t }
}

#[derive(Clone, Copy)]
struct D<const N: usize> {
    v: f64,
    g: [f64; N],
}

impl<const N: usize> D<N> {
    #[inline]
    const fn zero() -> Self {
        Self { v: 0.0, g: [0.0; N] }
    }

    #[inline]
    fn scaled_grad(v: f64, c: f64, g: &[f64; N]) -> Self {
        let mut out = Self { v, g: [0.0; N] };
        for (o, &x) in out.g.iter_mut().zip(g) {
            *o = mulz(c, x);
        }
        out
    }

    #[inline]
    fn mul(&self, o: &Self) -> Self {
        let mut out = Self { v: self.v * o.v, g: [0.0; N] };
        for ((r, &a), &b) in out.g.iter_mut().zip(&self.g).zip(&o.g) {
            *r = mulz(self.v, b) + mulz(o.v, a);
        }
        out
    }

    #[inline]
    fn add_assign(&mut self, o: &Self) {
        self.v += o.v;
        for (r, &b) in self.g.iter_mut().zip(&o.g) {
            *r += b;
        }
    }

    #[inline]
    fn axpy(&mut self, c: f64, o: &Self) {
        self.v += c * o.v;
        for (r, &b) in self.g.iter_mut().zip(&o.g) {
            *r += mulz(c, b);
        }
    }

    #[inline]
    fn is_zero(&self) -> bool {
        self.v == 0.0 && self.g.iter().all(|x| *x == 0.0)
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct Jet3<const N: usize> {
    v: [f64; 4],
    g: [[f64; N]; 4],
}

impl<const N: usize> Default for Jet3<N> {
    fn default() -> Self {
        Self::constant(0.0)
    }
}

impl<const N: usize> fmt::Debug for Jet3<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Jet3({:?}, ∇ {:?})", self.v, self.g[0])
    }
}

impl<const N: usize> From<f64> for Jet3<N> {
    fn from(v: f64) -> Self {
        Self::constant(v)
    }
}

impl<const N: usize> Jet3<N> {
    #[inline]
    #[must_use]
    pub const fn constant(v: f64) -> Self {
        Self { v: [v, 0.0, 0.0, 0.0], g: [[0.0; N]; 4] }
    }

    #[inline]
    #[must_use]
    pub const fn directed(v: f64, a: f64, b: f64) -> Self {
        Self { v: [v, a, b, 0.0], g: [[0.0; N]; 4] }
    }


    #[inline]
    #[must_use]
    pub fn variable(v: f64, k: usize, a: f64, b: f64) -> Self {
        let mut out = Self::directed(v, a, b);
        out.g[0][k] = 1.0;
        out
    }

    #[must_use]
    pub fn seed(values: &[f64; N], a: &[f64; N], b: &[f64; N]) -> [Self; N] {
        core::array::from_fn(|k| Self::variable(values[k], k, a[k], b[k]))
    }

    #[inline]
    fn part(&self, k: usize) -> D<N> {
        D { v: self.v[k], g: self.g[k] }
    }

    #[inline]
    fn from_parts(p: [D<N>; 4]) -> Self {
        Self { v: core::array::from_fn(|k| p[k].v), g: core::array::from_fn(|k| p[k].g) }
    }

    #[must_use]
    pub const fn primal(&self) -> f64 {
        self.v[0]
    }

    #[must_use]
    pub const fn gradient(&self) -> &[f64; N] {
        &self.g[0]
    }

    #[must_use]
    pub const fn d1(&self) -> f64 {
        self.v[1]
    }

    #[must_use]
    pub const fn d2(&self) -> f64 {
        self.v[2]
    }

    #[must_use]
    pub const fn d12(&self) -> f64 {
        self.v[3]
    }

    #[must_use]
    pub const fn gradient_d1(&self) -> &[f64; N] {
        &self.g[1]
    }

    #[must_use]
    pub const fn gradient_d2(&self) -> &[f64; N] {
        &self.g[2]
    }

    #[must_use]
    pub const fn third(&self) -> &[f64; N] {
        &self.g[3]
    }

    #[must_use]
    pub fn third_is_finite(&self) -> bool {
        self.v[3].is_finite() && self.g[3].iter().all(|x: &f64| x.is_finite())
    }

    #[inline]
    fn compose(&self, f0: &D<N>, f1: &D<N>, f2: &D<N>) -> Self {
        let (x1, x2, x12) = (self.part(1), self.part(2), self.part(3));
        let mut out12 = f1.mul(&x12);
        out12.add_assign(&f2.mul(&x1.mul(&x2)));
        Self::from_parts([*f0, f1.mul(&x1), f1.mul(&x2), out12])
    }

    #[inline]
    fn scaled(&self, c: f64) -> Self {
        let mut out = *self;
        for k in 0..4 {
            out.v[k] *= c;
            for x in &mut out.g[k] {
                *x = mulz(c, *x);
            }
        }
        out
    }
}

impl<const N: usize> Add for Jet3<N> {
    type Output = Self;
    #[inline]
    fn add(mut self, rhs: Self) -> Self {
        self += rhs;
        self
    }
}

impl<const N: usize> AddAssign for Jet3<N> {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        for k in 0..4 {
            self.v[k] += rhs.v[k];
            for (a, b) in self.g[k].iter_mut().zip(&rhs.g[k]) {
                *a += b;
            }
        }
    }
}

impl<const N: usize> Sub for Jet3<N> {
    type Output = Self;
    #[inline]
    fn sub(mut self, rhs: Self) -> Self {
        self -= rhs;
        self
    }
}

impl<const N: usize> SubAssign for Jet3<N> {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        for k in 0..4 {
            self.v[k] -= rhs.v[k];
            for (a, b) in self.g[k].iter_mut().zip(&rhs.g[k]) {
                *a -= b;
            }
        }
    }
}

impl<const N: usize> Mul for Jet3<N> {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: Self) -> Self {
        let (a0, a1, a2, a12) = (self.part(0), self.part(1), self.part(2), self.part(3));
        let (b0, b1, b2, b12) = (rhs.part(0), rhs.part(1), rhs.part(2), rhs.part(3));
        let mut p1 = a0.mul(&b1);
        p1.add_assign(&a1.mul(&b0));
        let mut p2 = a0.mul(&b2);
        p2.add_assign(&a2.mul(&b0));
        let mut p12 = a0.mul(&b12);
        p12.add_assign(&a12.mul(&b0));
        p12.add_assign(&a1.mul(&b2));
        p12.add_assign(&a2.mul(&b1));
        Self::from_parts([a0.mul(&b0), p1, p2, p12])
    }
}

impl<const N: usize> MulAssign for Jet3<N> {
    #[inline]
    fn mul_assign(&mut self, rhs: Self) {
        *self = *self * rhs;
    }
}

#[allow(clippy::suspicious_arithmetic_impl)]                                               
impl<const N: usize> Div for Jet3<N> {
    type Output = Self;
    #[inline]
    fn div(self, rhs: Self) -> Self {
        self * rhs.recip()
    }
}

impl<const N: usize> DivAssign for Jet3<N> {
    #[inline]
    fn div_assign(&mut self, rhs: Self) {
        *self = *self / rhs;
    }
}

impl<const N: usize> Neg for Jet3<N> {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        self.scaled(-1.0)
    }
}

impl<const N: usize> Add<f64> for Jet3<N> {
    type Output = Self;
    #[inline]
    fn add(mut self, rhs: f64) -> Self {
        self.v[0] += rhs;
        self
    }
}

impl<const N: usize> Sub<f64> for Jet3<N> {
    type Output = Self;
    #[inline]
    fn sub(mut self, rhs: f64) -> Self {
        self.v[0] -= rhs;
        self
    }
}

impl<const N: usize> Mul<f64> for Jet3<N> {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: f64) -> Self {
        self.scaled(rhs)
    }
}

#[allow(clippy::suspicious_arithmetic_impl)]
impl<const N: usize> Div<f64> for Jet3<N> {
    type Output = Self;
    #[inline]
    fn div(self, rhs: f64) -> Self {
        let mut out = self.scaled(1.0 / rhs);
        out.v[0] = self.v[0] / rhs;
        out
    }
}

impl<const N: usize> Sum for Jet3<N> {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        let mut acc = Self::constant(0.0);
        for x in iter {
            acc += x;
        }
        acc
    }
}

impl<const N: usize> Jet3<N> {
    fn poison_third(&mut self, grads: &[&[f64; N]]) {
        for (i, slot) in self.g[3].iter_mut().enumerate() {
            if grads.iter().any(|g| g[i] != 0.0) {
                *slot = f64::NAN;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn compose2<T: FnOnce() -> [f64; 4]>(
        a: Self,
        b: Self,
        f: f64,
        fa: f64,
        fb: f64,
        faa: f64,
        fab: f64,
        fbb: f64,
        third: Option<T>,
    ) -> Self {
        let (a1, a2, a12) = (a.part(1), a.part(2), a.part(3));
        let (b1, b2, b12) = (b.part(1), b.part(2), b.part(3));
        let products = [
            a1.mul(&a2),
            {
                let mut c = a1.mul(&b2);
                c.add_assign(&b1.mul(&a2));
                c
            },
            b1.mul(&b2),
        ];
        let moving = products.iter().any(|p| p.v != 0.0);
        let (ga, gb) = (&a.g[0], &b.g[0]);
        let lin =
            |c1: f64, c2: f64| -> [f64; N] { core::array::from_fn(|i| mulz(c1, ga[i]) + mulz(c2, gb[i])) };
        let known = third.is_some();
        let t = match third {
            Some(t) if moving => t(),
            _ => [0.0; 4],
        };
        let [t_aaa, t_aab, t_abb, t_bbb] = t;
        let f0 = D { v: f, g: lin(fa, fb) };
        let fda = D { v: fa, g: lin(faa, fab) };
        let fdb = D { v: fb, g: lin(fab, fbb) };
        let second = [
            D { v: faa, g: lin(t_aaa, t_aab) },
            D { v: fab, g: lin(t_aab, t_abb) },
            D { v: fbb, g: lin(t_abb, t_bbb) },
        ];
        let mut o1 = fda.mul(&a1);
        o1.add_assign(&fdb.mul(&b1));
        let mut o2 = fda.mul(&a2);
        o2.add_assign(&fdb.mul(&b2));
        let mut o12 = fda.mul(&a12);
        o12.add_assign(&fdb.mul(&b12));
        for (fd, p) in second.iter().zip(&products) {
            if !p.is_zero() {
                o12.add_assign(&fd.mul(p));
            }
        }
        let mut out = Self::from_parts([f0, o1, o2, o12]);
        if moving && !known {
            out.poison_third(&[ga, gb]);
        }
        out
    }

    fn lift_impl<T: FnOnce(&[f64], &[f64]) -> Vec<f64>>(
        f: f64,
        inputs: &[Self],
        grad: &[f64],
        hess: &[f64],
        third: Option<T>,
    ) -> Self {
        let n = inputs.len();
        let mut f0 = D { v: f, g: [0.0; N] };
        for (x, &gk) in inputs.iter().zip(grad) {
            f0.axpy(gk, &D { v: 0.0, g: x.g[0] });
        }
        let fk: Vec<D<N>> = (0..n)
            .map(|k| {
                let mut d = D { v: grad[k], g: [0.0; N] };
                if !hess.is_empty() {
                    for (l, x) in inputs.iter().enumerate() {
                        let h = hess[k * n + l];
                        if h != 0.0 {
                            d.axpy(h, &D { v: 0.0, g: x.g[0] });
                        }
                    }
                }
                d
            })
            .collect();
        let mut o1 = D::zero();
        let mut o2 = D::zero();
        let mut o12 = D::zero();
        for (k, x) in inputs.iter().enumerate() {
            o1.add_assign(&fk[k].mul(&x.part(1)));
            o2.add_assign(&fk[k].mul(&x.part(2)));
            o12.add_assign(&fk[k].mul(&x.part(3)));
        }
        if !hess.is_empty() {

            for k in 0..n {
                let xk1 = inputs[k].part(1);
                if xk1.is_zero() {
                    continue;
                }
                for l in 0..n {
                    let h = hess[k * n + l];
                    let xl2 = inputs[l].part(2);
                    if h != 0.0 && !xl2.is_zero() {
                        o12.axpy(h, &xk1.mul(&xl2));
                    }
                }
            }
        }
        let a: Vec<f64> = inputs.iter().map(|x| x.v[1]).collect();
        let b: Vec<f64> = inputs.iter().map(|x| x.v[2]).collect();
        let moving = a.iter().any(|x| *x != 0.0) && b.iter().any(|x| *x != 0.0);
        let mut out_known = true;
        if moving {
            if let Some(t) = third {
                let tm = t(&a, &b);
                assert!(
                    tm.len() == n,
                    "Scalar::lift3: the third-derivative contraction must have {n} entries"
                );
                for (x, &c) in inputs.iter().zip(&tm) {
                    o12.axpy(c, &D { v: 0.0, g: x.g[0] });
                }
            } else {
                out_known = false;
            }
        }
        let mut out = Self::from_parts([f0, o1, o2, o12]);
        if !out_known {
            let grads: Vec<&[f64; N]> = inputs.iter().map(|x| &x.g[0]).collect();
            out.poison_third(&grads);
        }
        out
    }
}

impl<const N: usize> Scalar for Jet3<N> {
    #[inline]
    fn from_f64(value: f64) -> Self {
        Self::constant(value)
    }

    #[inline]
    fn value(&self) -> f64 {
        self.v[0]
    }

    #[inline]
    fn chain(self, f: f64, df: f64, d2f: f64) -> Self {
        let g0 = &self.g[0];
        let f0 = D::scaled_grad(f, df, g0);
        let f1 = D::scaled_grad(df, d2f, g0);
        let mut out = self.compose(&f0, &f1, &D { v: d2f, g: [0.0; N] });
        if self.v[1] * self.v[2] != 0.0 {
            out.poison_third(&[g0]);
        }
        out
    }

    #[inline]
    fn chain3(self, f: f64, df: f64, d2f: f64, d3f: impl FnOnce() -> f64) -> Self {
        let g0 = &self.g[0];
        let f0 = D::scaled_grad(f, df, g0);
        let f1 = D::scaled_grad(df, d2f, g0);
        let f2 = if self.v[1] * self.v[2] == 0.0 {
            D { v: d2f, g: [0.0; N] }
        } else {
            D::scaled_grad(d2f, d3f(), g0)
        };
        self.compose(&f0, &f1, &f2)
    }

    #[inline]
    fn chain2(a: Self, b: Self, f: f64, fa: f64, fb: f64, faa: f64, fab: f64, fbb: f64) -> Self {
        Self::compose2(a, b, f, fa, fb, faa, fab, fbb, None::<fn() -> [f64; 4]>)
    }

    #[inline]
    fn chain2_3(
        a: Self,
        b: Self,
        f: f64,
        fa: f64,
        fb: f64,
        faa: f64,
        fab: f64,
        fbb: f64,
        third: impl FnOnce() -> [f64; 4],
    ) -> Self {
        Self::compose2(a, b, f, fa, fb, faa, fab, fbb, Some(third))
    }

    fn lift(f: f64, inputs: &[Self], grad: &[f64], hess: &[f64]) -> Self {
        check_lift_shapes(inputs.len(), grad.len(), hess.len());
        Self::lift_impl(f, inputs, grad, hess, None::<fn(&[f64], &[f64]) -> Vec<f64>>)
    }

    fn lift3(
        f: f64,
        inputs: &[Self],
        grad: &[f64],
        hess: &[f64],
        third: impl FnOnce(&[f64], &[f64]) -> Vec<f64>,
    ) -> Self {
        check_lift_shapes(inputs.len(), grad.len(), hess.len());
        Self::lift_impl(f, inputs, grad, hess, Some(third))
    }
}

