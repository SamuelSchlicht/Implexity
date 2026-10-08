// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use core::fmt;
use core::iter::Sum;
use core::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

use implexity_ad::Scalar;

#[inline]
fn mulz(c: f64, t: f64) -> f64 {
    if t == 0.0 { 0.0 } else { c * t }
}

#[derive(Clone, Copy, PartialEq)]
pub struct Jet2<const N: usize> {
    pub v: f64,
    pub g: [f64; N],
    h: [[f64; N]; N],
}

impl<const N: usize> Default for Jet2<N> {
    fn default() -> Self {
        Self::constant(0.0)
    }
}

impl<const N: usize> fmt::Debug for Jet2<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Jet2({}, {:?})", self.v, self.g)
    }
}

impl<const N: usize> Jet2<N> {
    #[inline]
    #[must_use]
    pub const fn constant(v: f64) -> Self {
        Self { v, g: [0.0; N], h: [[0.0; N]; N] }
    }


    #[inline]
    #[must_use]
    pub fn variable(v: f64, k: usize) -> Self {
        let mut out = Self::constant(v);
        out.g[k] = 1.0;
        out
    }

    #[must_use]
    pub fn seed(values: &[f64; N]) -> [Self; N] {
        core::array::from_fn(|k| Self::variable(values[k], k))
    }

    #[must_use]
    pub fn hessian(&self) -> [[f64; N]; N] {
        let mut out = [[0.0; N]; N];
        for i in 0..N {
            for j in i..N {
                out[i][j] = self.h[i][j];
                out[j][i] = self.h[i][j];
            }
        }
        out
    }

    #[inline]
    fn add_sym_outer(h: &mut [[f64; N]; N], c: f64, a: &[f64; N], b: &[f64; N]) {
        if c == 0.0 {
            return;
        }
        for i in 0..N {
            let (ai, bi) = (a[i], b[i]);
            if ai == 0.0 && bi == 0.0 {
                continue;
            }
            for j in i..N {
                h[i][j] += c * (ai * b[j] + a[j] * bi);
            }
        }
    }

    #[inline]
    fn add_outer(h: &mut [[f64; N]; N], c: f64, a: &[f64; N]) {
        if c == 0.0 {
            return;
        }
        for i in 0..N {
            let ai = a[i];
            if ai == 0.0 {
                continue;
            }
            let cai = c * ai;
            for j in i..N {
                h[i][j] += cai * a[j];
            }
        }
    }

    #[inline]
    fn scaled(&self, c: f64) -> Self {
        let mut out = *self;
        out.v *= c;
        for i in 0..N {
            out.g[i] = mulz(c, self.g[i]);
            for j in i..N {
                out.h[i][j] = mulz(c, self.h[i][j]);
            }
        }
        out
    }
}

impl<const N: usize> Add for Jet2<N> {
    type Output = Self;
    #[inline]
    fn add(mut self, rhs: Self) -> Self {
        self += rhs;
        self
    }
}

impl<const N: usize> AddAssign for Jet2<N> {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.v += rhs.v;
        for i in 0..N {
            self.g[i] += rhs.g[i];
            for j in i..N {
                self.h[i][j] += rhs.h[i][j];
            }
        }
    }
}

impl<const N: usize> Sub for Jet2<N> {
    type Output = Self;
    #[inline]
    fn sub(mut self, rhs: Self) -> Self {
        self -= rhs;
        self
    }
}

impl<const N: usize> SubAssign for Jet2<N> {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.v -= rhs.v;
        for i in 0..N {
            self.g[i] -= rhs.g[i];
            for j in i..N {
                self.h[i][j] -= rhs.h[i][j];
            }
        }
    }
}

impl<const N: usize> Mul for Jet2<N> {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: Self) -> Self {
        let mut out = Self::constant(self.v * rhs.v);
        for i in 0..N {
            out.g[i] = mulz(self.v, rhs.g[i]) + mulz(rhs.v, self.g[i]);
            for j in i..N {
                out.h[i][j] = mulz(self.v, rhs.h[i][j]) + mulz(rhs.v, self.h[i][j]);
            }
        }
        Self::add_sym_outer(&mut out.h, 1.0, &self.g, &rhs.g);
        out
    }
}

impl<const N: usize> MulAssign for Jet2<N> {
    #[inline]
    fn mul_assign(&mut self, rhs: Self) {
        *self = *self * rhs;
    }
}

#[allow(clippy::suspicious_arithmetic_impl)]                                               
impl<const N: usize> Div for Jet2<N> {
    type Output = Self;
    #[inline]
    fn div(self, rhs: Self) -> Self {
        self * rhs.recip()
    }
}

impl<const N: usize> DivAssign for Jet2<N> {
    #[inline]
    fn div_assign(&mut self, rhs: Self) {
        *self = *self / rhs;
    }
}

impl<const N: usize> Neg for Jet2<N> {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        self.scaled(-1.0)
    }
}

impl<const N: usize> Add<f64> for Jet2<N> {
    type Output = Self;
    #[inline]
    fn add(mut self, rhs: f64) -> Self {
        self.v += rhs;
        self
    }
}

impl<const N: usize> Sub<f64> for Jet2<N> {
    type Output = Self;
    #[inline]
    fn sub(mut self, rhs: f64) -> Self {
        self.v -= rhs;
        self
    }
}

impl<const N: usize> Mul<f64> for Jet2<N> {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: f64) -> Self {
        self.scaled(rhs)
    }
}

#[allow(clippy::suspicious_arithmetic_impl)]
impl<const N: usize> Div<f64> for Jet2<N> {
    type Output = Self;
    #[inline]
    fn div(self, rhs: f64) -> Self {
        let mut out = self.scaled(1.0 / rhs);
        out.v = self.v / rhs;
        out
    }
}

impl<const N: usize> Sum for Jet2<N> {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        let mut acc = Self::constant(0.0);
        for x in iter {
            acc += x;
        }
        acc
    }
}

impl<const N: usize> From<f64> for Jet2<N> {
    fn from(v: f64) -> Self {
        Self::constant(v)
    }
}

impl<const N: usize> Scalar for Jet2<N> {
    #[inline]
    fn from_f64(value: f64) -> Self {
        Self::constant(value)
    }

    #[inline]
    fn value(&self) -> f64 {
        self.v
    }

    #[inline]
    fn chain(self, f: f64, df: f64, d2f: f64) -> Self {
        let mut out = Self::constant(f);
        for i in 0..N {
            out.g[i] = mulz(df, self.g[i]);
            for j in i..N {
                out.h[i][j] = mulz(df, self.h[i][j]);
            }
        }
        Self::add_outer(&mut out.h, d2f, &self.g);
        out
    }

    #[inline]
    fn chain2(a: Self, b: Self, f: f64, fa: f64, fb: f64, faa: f64, fab: f64, fbb: f64) -> Self {
        let mut out = Self::constant(f);
        for i in 0..N {
            out.g[i] = mulz(fa, a.g[i]) + mulz(fb, b.g[i]);
            for j in i..N {
                out.h[i][j] = mulz(fa, a.h[i][j]) + mulz(fb, b.h[i][j]);
            }
        }
        Self::add_outer(&mut out.h, faa, &a.g);
        Self::add_sym_outer(&mut out.h, fab, &a.g, &b.g);
        Self::add_outer(&mut out.h, fbb, &b.g);
        out
    }

    fn lift(f: f64, inputs: &[Self], grad: &[f64], hess: &[f64]) -> Self {
        let n = inputs.len();
        assert!(
            grad.len() == n && (hess.is_empty() || hess.len() == n * n),
            "Jet2::lift: {n} inputs need {n} gradient and 0 or {} Hessian entries",
            n * n
        );
        let mut out = Self::constant(f);
        for (x, &gk) in inputs.iter().zip(grad) {
            for i in 0..N {
                out.g[i] += mulz(gk, x.g[i]);
                for j in i..N {
                    out.h[i][j] += mulz(gk, x.h[i][j]);
                }
            }
        }
        if !hess.is_empty() {
            for k in 0..n {
                let hkk = hess[k * n + k];
                Self::add_outer(&mut out.h, hkk, &inputs[k].g);
                for l in k + 1..n {

                    let c = 0.5 * (hess[k * n + l] + hess[l * n + k]);
                    Self::add_sym_outer(&mut out.h, c, &inputs[k].g, &inputs[l].g);
                }
            }
        }
        out
    }
}

