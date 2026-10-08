// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use core::fmt;
use core::iter::Sum;
use core::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

use crate::scalar::{Scalar, check_lift_shapes};


#[derive(Clone, Copy, PartialEq)]
pub struct Dual<const N: usize> {
    pub re: f64,
    pub eps: [f64; N],
}

impl<const N: usize> Dual<N> {
    #[inline]
    #[must_use]
    pub const fn constant(re: f64) -> Self {
        Self { re, eps: [0.0; N] }
    }



    #[inline]
    #[must_use]
    pub fn variable(re: f64, k: usize) -> Self {
        let mut eps = [0.0; N];
        eps[k] = 1.0;
        Self { re, eps }
    }

    #[inline]
    #[must_use]
    pub const fn new(re: f64, eps: [f64; N]) -> Self {
        Self { re, eps }
    }

    #[inline]
    fn map_eps(self, f: impl Fn(f64) -> f64) -> [f64; N] {
        let mut out = [0.0; N];
        for (o, &e) in out.iter_mut().zip(&self.eps) {
            *o = f(e);
        }
        out
    }
}

impl<const N: usize> Default for Dual<N> {
    fn default() -> Self {
        Self::constant(0.0)
    }
}

impl<const N: usize> fmt::Debug for Dual<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Dual({:?}, {:?})", self.re, self.eps)
    }
}

impl<const N: usize> fmt::Display for Dual<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.re)?;
        for (k, e) in self.eps.iter().enumerate() {
            write!(f, " {e:+}ε{k}")?;
        }
        Ok(())
    }
}

impl<const N: usize> Add for Dual<N> {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self {
        let mut eps = self.eps;
        for (e, r) in eps.iter_mut().zip(&rhs.eps) {
            *e += r;
        }
        Self { re: self.re + rhs.re, eps }
    }
}

impl<const N: usize> Sub for Dual<N> {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self {
        let mut eps = self.eps;
        for (e, r) in eps.iter_mut().zip(&rhs.eps) {
            *e -= r;
        }
        Self { re: self.re - rhs.re, eps }
    }
}

impl<const N: usize> Mul for Dual<N> {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: Self) -> Self {
        let mut eps = [0.0; N];
        for ((e, a), b) in eps.iter_mut().zip(&self.eps).zip(&rhs.eps) {
            *e = a * rhs.re + self.re * b;
        }
        Self { re: self.re * rhs.re, eps }
    }
}

impl<const N: usize> Div for Dual<N> {
    type Output = Self;
    #[inline]
    fn div(self, rhs: Self) -> Self {
        let inv = 1.0 / rhs.re;
        let q = self.re * inv;
        let mut eps = [0.0; N];
        for ((e, a), b) in eps.iter_mut().zip(&self.eps).zip(&rhs.eps) {
            *e = a / rhs.re - b * q * inv;
        }
        Self { re: self.re / rhs.re, eps }
    }
}

impl<const N: usize> Neg for Dual<N> {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Self { re: -self.re, eps: self.map_eps(|e| -e) }
    }
}

impl<const N: usize> Add<f64> for Dual<N> {
    type Output = Self;
    #[inline]
    fn add(self, rhs: f64) -> Self {
        Self { re: self.re + rhs, eps: self.eps }
    }
}

impl<const N: usize> Sub<f64> for Dual<N> {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: f64) -> Self {
        Self { re: self.re - rhs, eps: self.eps }
    }
}

impl<const N: usize> Mul<f64> for Dual<N> {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: f64) -> Self {
        Self { re: self.re * rhs, eps: self.map_eps(|e| e * rhs) }
    }
}

impl<const N: usize> Div<f64> for Dual<N> {
    type Output = Self;
    #[inline]
    fn div(self, rhs: f64) -> Self {
        Self { re: self.re / rhs, eps: self.map_eps(|e| e / rhs) }
    }
}

macro_rules! assign_ops {
    ($t:ty) => {
        impl<const N: usize> AddAssign for $t {
            #[inline]
            fn add_assign(&mut self, rhs: Self) {
                *self = *self + rhs;
            }
        }
        impl<const N: usize> SubAssign for $t {
            #[inline]
            fn sub_assign(&mut self, rhs: Self) {
                *self = *self - rhs;
            }
        }
        impl<const N: usize> MulAssign for $t {
            #[inline]
            fn mul_assign(&mut self, rhs: Self) {
                *self = *self * rhs;
            }
        }
        impl<const N: usize> DivAssign for $t {
            #[inline]
            fn div_assign(&mut self, rhs: Self) {
                *self = *self / rhs;
            }
        }
        impl<const N: usize> Sum for $t {
            fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
                let mut acc = Self::default();
                for x in iter {
                    acc += x;
                }
                acc
            }
        }
    };
}
assign_ops!(Dual<N>);

impl<const N: usize> From<f64> for Dual<N> {
    fn from(value: f64) -> Self {
        Self::constant(value)
    }
}

impl<const N: usize> Scalar for Dual<N> {
    #[inline]
    fn from_f64(value: f64) -> Self {
        Self::constant(value)
    }

    #[inline]
    fn value(&self) -> f64 {
        self.re
    }

    #[inline]
    fn chain(self, f: f64, df: f64, _d2f: f64) -> Self {

        Self { re: f, eps: self.map_eps(|e| if e == 0.0 { 0.0 } else { df * e }) }
    }

    #[inline]
    fn chain2(a: Self, b: Self, f: f64, fa: f64, fb: f64, _faa: f64, _fab: f64, _fbb: f64) -> Self {
        let mut eps = [0.0; N];
        for ((e, &ea), &eb) in eps.iter_mut().zip(&a.eps).zip(&b.eps) {

            let ta = if ea == 0.0 { 0.0 } else { fa * ea };
            let tb = if eb == 0.0 { 0.0 } else { fb * eb };
            *e = ta + tb;
        }
        Self { re: f, eps }
    }

    fn lift(f: f64, inputs: &[Self], grad: &[f64], hess: &[f64]) -> Self {
        check_lift_shapes(inputs.len(), grad.len(), hess.len());
        let mut eps = [0.0; N];
        for (x, &g) in inputs.iter().zip(grad) {
            for (e, &xe) in eps.iter_mut().zip(&x.eps) {
                if xe != 0.0 {
                    *e += g * xe;
                }
            }
        }
        Self { re: f, eps }
    }
}
