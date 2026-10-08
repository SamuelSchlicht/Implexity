// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use core::fmt;
use core::iter::Sum;
use core::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

use crate::scalar::{Scalar, check_lift_shapes};


#[derive(Clone, Copy, PartialEq, Default)]
pub struct HyperDual {
    pub re: f64,
    pub e1: f64,
    pub e2: f64,
    pub e12: f64,
}

#[inline]
fn mulz(c: f64, t: f64) -> f64 {
    if t == 0.0 { 0.0 } else { c * t }
}

impl HyperDual {
    #[inline]
    #[must_use]
    pub const fn new(re: f64, e1: f64, e2: f64, e12: f64) -> Self {
        Self { re, e1, e2, e12 }
    }

    #[inline]
    #[must_use]
    pub const fn constant(re: f64) -> Self {
        Self { re, e1: 0.0, e2: 0.0, e12: 0.0 }
    }
}

impl fmt::Debug for HyperDual {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HyperDual({:?}, {:?}, {:?}, {:?})", self.re, self.e1, self.e2, self.e12)
    }
}

impl fmt::Display for HyperDual {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {:+}ε₁ {:+}ε₂ {:+}ε₁ε₂", self.re, self.e1, self.e2, self.e12)
    }
}

impl Add for HyperDual {
    type Output = Self;
    #[inline]
    fn add(self, r: Self) -> Self {
        Self::new(self.re + r.re, self.e1 + r.e1, self.e2 + r.e2, self.e12 + r.e12)
    }
}

impl Sub for HyperDual {
    type Output = Self;
    #[inline]
    fn sub(self, r: Self) -> Self {
        Self::new(self.re - r.re, self.e1 - r.e1, self.e2 - r.e2, self.e12 - r.e12)
    }
}

impl Mul for HyperDual {
    type Output = Self;
    #[inline]
    fn mul(self, r: Self) -> Self {
        Self::new(
            self.re * r.re,
            self.e1 * r.re + self.re * r.e1,
            self.e2 * r.re + self.re * r.e2,
            self.e12 * r.re + self.e1 * r.e2 + self.e2 * r.e1 + self.re * r.e12,
        )
    }
}

#[allow(clippy::suspicious_arithmetic_impl)]                                               
impl Div for HyperDual {
    type Output = Self;
    #[inline]
    fn div(self, r: Self) -> Self {
        self * r.recip()
    }
}

impl Neg for HyperDual {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Self::new(-self.re, -self.e1, -self.e2, -self.e12)
    }
}

impl Add<f64> for HyperDual {
    type Output = Self;
    #[inline]
    fn add(self, r: f64) -> Self {
        Self { re: self.re + r, ..self }
    }
}

impl Sub<f64> for HyperDual {
    type Output = Self;
    #[inline]
    fn sub(self, r: f64) -> Self {
        Self { re: self.re - r, ..self }
    }
}

impl Mul<f64> for HyperDual {
    type Output = Self;
    #[inline]
    fn mul(self, r: f64) -> Self {
        Self::new(self.re * r, self.e1 * r, self.e2 * r, self.e12 * r)
    }
}

impl Div<f64> for HyperDual {
    type Output = Self;
    #[inline]
    fn div(self, r: f64) -> Self {
        Self::new(self.re / r, self.e1 / r, self.e2 / r, self.e12 / r)
    }
}

impl AddAssign for HyperDual {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}
impl SubAssign for HyperDual {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}
impl MulAssign for HyperDual {
    #[inline]
    fn mul_assign(&mut self, rhs: Self) {
        *self = *self * rhs;
    }
}
impl DivAssign for HyperDual {
    #[inline]
    fn div_assign(&mut self, rhs: Self) {
        *self = *self / rhs;
    }
}
impl Sum for HyperDual {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        let mut acc = Self::default();
        for x in iter {
            acc += x;
        }
        acc
    }
}

impl From<f64> for HyperDual {
    fn from(value: f64) -> Self {
        Self::constant(value)
    }
}

impl Scalar for HyperDual {
    #[inline]
    fn from_f64(value: f64) -> Self {
        Self::constant(value)
    }

    #[inline]
    fn value(&self) -> f64 {
        self.re
    }

    #[inline]
    fn chain(self, f: f64, df: f64, d2f: f64) -> Self {
        Self::new(f, mulz(df, self.e1), mulz(df, self.e2), mulz(df, self.e12) + mulz(d2f, self.e1 * self.e2))
    }

    #[inline]
    fn chain2(a: Self, b: Self, f: f64, fa: f64, fb: f64, faa: f64, fab: f64, fbb: f64) -> Self {
        Self::new(
            f,
            mulz(fa, a.e1) + mulz(fb, b.e1),
            mulz(fa, a.e2) + mulz(fb, b.e2),
            mulz(fa, a.e12)
                + mulz(fb, b.e12)
                + mulz(faa, a.e1 * a.e2)
                + mulz(fab, a.e1 * b.e2 + b.e1 * a.e2)
                + mulz(fbb, b.e1 * b.e2),
        )
    }

    fn lift(f: f64, inputs: &[Self], grad: &[f64], hess: &[f64]) -> Self {
        check_lift_shapes(inputs.len(), grad.len(), hess.len());
        let n = inputs.len();
        let mut out = Self::constant(f);
        for (x, &g) in inputs.iter().zip(grad) {
            out.e1 += mulz(g, x.e1);
            out.e2 += mulz(g, x.e2);
            out.e12 += mulz(g, x.e12);
        }
        if !hess.is_empty() {
            for i in 0..n {
                for j in 0..n {
                    out.e12 += mulz(hess[i * n + j], inputs[i].e1 * inputs[j].e2);
                }
            }
        }
        out
    }
}
