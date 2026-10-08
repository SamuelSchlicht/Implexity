// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use core::fmt::Debug;
use core::iter::Sum;
use core::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};


pub trait Scalar:
    Copy
    + Debug
    + Default
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
    + AddAssign
    + SubAssign
    + MulAssign
    + DivAssign
    + Sum
{
    fn from_f64(value: f64) -> Self;

    fn value(&self) -> f64;

    #[must_use]
    fn chain(self, f: f64, df: f64, d2f: f64) -> Self;

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    fn chain2(a: Self, b: Self, f: f64, fa: f64, fb: f64, faa: f64, fab: f64, fbb: f64) -> Self;



    #[must_use]
    fn lift(f: f64, inputs: &[Self], grad: &[f64], hess: &[f64]) -> Self;

    #[inline]
    #[must_use]
    fn chain3(self, f: f64, df: f64, d2f: f64, d3f: impl FnOnce() -> f64) -> Self {
        let _ = d3f;
        self.chain(f, df, d2f)
    }

    #[inline]
    #[must_use]
    #[allow(clippy::too_many_arguments)]
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
        let _ = third;
        Self::chain2(a, b, f, fa, fb, faa, fab, fbb)
    }



    #[inline]
    #[must_use]
    fn lift3(
        f: f64,
        inputs: &[Self],
        grad: &[f64],
        hess: &[f64],
        third: impl FnOnce(&[f64], &[f64]) -> Vec<f64>,
    ) -> Self {
        let _ = third;
        Self::lift(f, inputs, grad, hess)
    }

    #[must_use]
    fn zero() -> Self {
        Self::from_f64(0.0)
    }

    #[must_use]
    fn one() -> Self {
        Self::from_f64(1.0)
    }

    #[must_use]
    fn exp(self) -> Self {
        let e = self.value().exp();
        self.chain3(e, e, e, || e)
    }

    #[must_use]
    fn exp_m1(self) -> Self {
        let x = self.value();
        let e = x.exp();
        self.chain3(x.exp_m1(), e, e, || e)
    }

    #[must_use]
    fn ln(self) -> Self {
        let x = self.value();
        self.chain3(x.ln(), 1.0 / x, -1.0 / (x * x), || 2.0 / (x * x * x))
    }

    #[must_use]
    fn ln_1p(self) -> Self {
        let x = self.value();
        let d = 1.0 / (x + 1.0);
        self.chain3(x.ln_1p(), d, -d * d, || 2.0 * d * d * d)
    }

    #[must_use]
    fn log10(self) -> Self {
        let x = self.value();
        let k = core::f64::consts::LN_10;
        self.chain3(x.log10(), 1.0 / (x * k), -1.0 / (x * x * k), || 2.0 / (x * x * x * k))
    }

    #[must_use]
    fn log2(self) -> Self {
        let x = self.value();
        let k = core::f64::consts::LN_2;
        self.chain3(x.log2(), 1.0 / (x * k), -1.0 / (x * x * k), || 2.0 / (x * x * x * k))
    }

    #[must_use]
    fn sqrt(self) -> Self {
        let x = self.value();
        let s = x.sqrt();
        let d = 0.5 / s;
        self.chain3(s, d, -0.5 * d / x, || 0.75 * d / (x * x))
    }

    #[must_use]
    fn cbrt(self) -> Self {
        let x = self.value();
        let c = x.cbrt();
        let d = 1.0 / (3.0 * c * c);
        self.chain3(c, d, -2.0 * d / (3.0 * x), || 10.0 * d / (9.0 * x * x))
    }

    #[must_use]
    fn square(self) -> Self {
        self * self
    }

    #[must_use]
    fn recip(self) -> Self {
        let x = self.value();
        let r = 1.0 / x;
        self.chain3(r, -r * r, 2.0 * r * r * r, || -6.0 * r * r * r * r)
    }

    #[must_use]
    fn powi(self, n: i32) -> Self {
        let x = self.value();
        match n {
            0 => Self::one(),
            1 => self,
            2 => self * self,
            _ => {
                let nf = f64::from(n);
                self.chain3(x.powi(n), nf * x.powi(n - 1), nf * (nf - 1.0) * x.powi(n - 2), || {
                    nf * (nf - 1.0) * (nf - 2.0) * x.powi(n - 3)
                })
            }
        }
    }

    #[must_use]
    fn powf(self, y: f64) -> Self {
        let x = self.value();
        self.chain3(x.powf(y), y * x.powf(y - 1.0), y * (y - 1.0) * x.powf(y - 2.0), || {
            y * (y - 1.0) * (y - 2.0) * x.powf(y - 3.0)
        })
    }

    #[must_use]
    fn pow(self, y: Self) -> Self {
        let (xv, yv) = (self.value(), y.value());
        let z = xv.powf(yv);
        let lx = if xv == 0.0 { 0.0 } else { xv.ln() };
        let fx = yv * xv.powf(yv - 1.0);
        let fy = lx * z;
        let fxx = yv * (yv - 1.0) * xv.powf(yv - 2.0);
        let fxy = xv.powf(yv - 1.0) * (1.0 + yv * lx);
        let fyy = lx * lx * z;
        Self::chain2_3(self, y, z, fx, fy, fxx, fxy, fyy, || {
            [
                yv * (yv - 1.0) * (yv - 2.0) * xv.powf(yv - 3.0),
                xv.powf(yv - 2.0) * (2.0 * yv - 1.0 + yv * (yv - 1.0) * lx),
                xv.powf(yv - 1.0) * lx * (2.0 + yv * lx),
                lx * lx * lx * z,
            ]
        })
    }

    #[must_use]
    fn sin(self) -> Self {
        let (s, c) = self.value().sin_cos();
        self.chain3(s, c, -s, || -c)
    }

    #[must_use]
    fn cos(self) -> Self {
        let (s, c) = self.value().sin_cos();
        self.chain3(c, -s, -c, || s)
    }

    #[must_use]
    fn tan(self) -> Self {
        let t = self.value().tan();
        let d = 1.0 + t * t;
        self.chain3(t, d, 2.0 * t * d, || 2.0 * d * (d + 2.0 * t * t))
    }

    #[must_use]
    fn asin(self) -> Self {
        let x = self.value();
        let r = 1.0 / (1.0 - x * x).sqrt();
        self.chain3(x.asin(), r, x * r * r * r, || r * r * r * (1.0 + 3.0 * x * x * r * r))
    }

    #[must_use]
    fn acos(self) -> Self {
        let x = self.value();
        let r = 1.0 / (1.0 - x * x).sqrt();
        self.chain3(x.acos(), -r, -x * r * r * r, || -r * r * r * (1.0 + 3.0 * x * x * r * r))
    }

    #[must_use]
    fn atan(self) -> Self {
        let x = self.value();
        let d = 1.0 / (1.0 + x * x);
        self.chain3(x.atan(), d, -2.0 * x * d * d, || d * d * (8.0 * x * x * d - 2.0))
    }

    #[must_use]
    fn atan2(self, x: Self) -> Self {
        let (yv, xv) = (self.value(), x.value());
        let r2 = xv * xv + yv * yv;
        let fy = xv / r2;
        let fx = -yv / r2;
        let r4 = r2 * r2;
        let fyy = -2.0 * xv * yv / r4;
        let fxx = 2.0 * xv * yv / r4;
        let fyx = (yv * yv - xv * xv) / r4;
        Self::chain2_3(self, x, yv.atan2(xv), fy, fx, fyy, fyx, fxx, || {
            let r6 = r4 * r2;
            [
                2.0 * xv * (3.0 * yv * yv - xv * xv) / r6,
                2.0 * yv * (3.0 * xv * xv - yv * yv) / r6,
                2.0 * xv * (xv * xv - 3.0 * yv * yv) / r6,
                2.0 * yv * (yv * yv - 3.0 * xv * xv) / r6,
            ]
        })
    }

    #[must_use]
    fn sinh(self) -> Self {
        let x = self.value();
        let (s, c) = (x.sinh(), x.cosh());
        self.chain3(s, c, s, || c)
    }

    #[must_use]
    fn cosh(self) -> Self {
        let x = self.value();
        let (s, c) = (x.sinh(), x.cosh());
        self.chain3(c, s, c, || s)
    }

    #[must_use]
    fn tanh(self) -> Self {
        let t = self.value().tanh();
        let d = 1.0 - t * t;
        self.chain3(t, d, -2.0 * t * d, || d * (4.0 * t * t - 2.0 * d))
    }

    #[must_use]
    fn asinh(self) -> Self {
        let x = self.value();
        let r = 1.0 / (x * x + 1.0).sqrt();
        self.chain3(x.asinh(), r, -x * r * r * r, || r * r * r * (3.0 * x * x * r * r - 1.0))
    }

    #[must_use]
    fn acosh(self) -> Self {
        let x = self.value();
        let r = 1.0 / (x * x - 1.0).sqrt();
        self.chain3(x.acosh(), r, -x * r * r * r, || r * r * r * (3.0 * x * x * r * r - 1.0))
    }

    #[must_use]
    fn atanh(self) -> Self {
        let x = self.value();
        let d = 1.0 / (1.0 - x * x);
        self.chain3(x.atanh(), d, 2.0 * x * d * d, || d * d * (2.0 + 8.0 * x * x * d))
    }

    #[must_use]
    fn abs(self) -> Self {
        let x = self.value();
        if x >= 0.0 { self.chain3(x, 1.0, 0.0, || 0.0) } else { self.chain3(-x, -1.0, 0.0, || 0.0) }
    }

    #[must_use]
    fn sign(self) -> Self {
        let x = self.value();
        let s = if x > 0.0 {
            1.0
        } else if x < 0.0 {
            -1.0
        } else {
            x
        };
        Self::from_f64(s)
    }

    #[must_use]
    fn floor(self) -> Self {
        Self::from_f64(self.value().floor())
    }

    #[must_use]
    fn ceil(self) -> Self {
        Self::from_f64(self.value().ceil())
    }

    #[must_use]
    fn round(self) -> Self {
        Self::from_f64(self.value().round_ties_even())
    }

    #[must_use]
    #[allow(clippy::float_cmp)]                                
    fn maximum(self, other: Self) -> Self {
        let (a, b) = (self.value(), other.value());
        if a > b {
            self
        } else if b > a {
            other
        } else if a == b {
            Self::chain2_3(self, other, a, 0.5, 0.5, 0.0, 0.0, 0.0, || [0.0; 4])
        } else {

            Self::from_f64(f64::NAN)
        }
    }

    #[must_use]
    #[allow(clippy::float_cmp)]                                
    fn minimum(self, other: Self) -> Self {
        let (a, b) = (self.value(), other.value());
        if a < b {
            self
        } else if b < a {
            other
        } else if a == b {
            Self::chain2_3(self, other, a, 0.5, 0.5, 0.0, 0.0, 0.0, || [0.0; 4])
        } else {
            Self::from_f64(f64::NAN)
        }
    }

    #[must_use]
    fn max_f64(self, c: f64) -> Self {
        self.maximum(Self::from_f64(c))
    }

    #[must_use]
    fn min_f64(self, c: f64) -> Self {
        self.minimum(Self::from_f64(c))
    }

    #[must_use]
    fn clip(self, lo: f64, hi: f64) -> Self {
        self.max_f64(lo).min_f64(hi)
    }

    #[must_use]
    fn sigmoid(self) -> Self {
        let x = self.value();
        let s = 1.0 / (1.0 + (-x).exp());
        let d = s * (1.0 - s);
        self.chain3(s, d, d * (1.0 - 2.0 * s), || d * (1.0 - 2.0 * s) * (1.0 - 2.0 * s) - 2.0 * d * d)
    }

    #[must_use]
    fn logaddexp(self, other: Self) -> Self {
        let (a, b) = (self.value(), other.value());
        let amax = a.max(b);
        let delta = a - b;
        let out = if delta.is_nan() { a + b } else { amax + (-delta.abs()).exp().ln_1p() };
        let pa = (a - out).exp();
        let pb = (b - out).exp();
        Self::chain2_3(self, other, out, pa, pb, pa * pb, -pa * pb, pa * pb, || {
            let c = pa * pb * (pb - pa);
            [c, -c, c, -c]
        })
    }

    #[must_use]
    fn softplus(self) -> Self {
        self.logaddexp(Self::zero())
    }

    #[must_use]
    fn hypot(self, other: Self) -> Self {
        let (a, b) = (self.value(), other.value());
        let h = a.hypot(b);
        let h3 = h * h * h;
        Self::chain2_3(self, other, h, a / h, b / h, b * b / h3, -a * b / h3, a * a / h3, || {
            let h5 = h3 * h * h;
            [
                -3.0 * a * b * b / h5,
                b * (2.0 * a * a - b * b) / h5,
                a * (2.0 * b * b - a * a) / h5,
                -3.0 * b * a * a / h5,
            ]
        })
    }

    #[must_use]
    fn stop_gradient(self) -> Self {
        Self::from_f64(self.value())
    }

    fn is_finite(&self) -> bool {
        self.value().is_finite()
    }
}

impl Scalar for f64 {
    #[inline]
    fn from_f64(value: f64) -> Self {
        value
    }

    #[inline]
    fn value(&self) -> f64 {
        *self
    }

    #[inline]
    fn chain(self, f: f64, _df: f64, _d2f: f64) -> Self {
        f
    }

    #[inline]
    fn chain2(_a: Self, _b: Self, f: f64, _fa: f64, _fb: f64, _faa: f64, _fab: f64, _fbb: f64) -> Self {
        f
    }

    #[inline]
    fn lift(f: f64, inputs: &[Self], grad: &[f64], hess: &[f64]) -> Self {
        check_lift_shapes(inputs.len(), grad.len(), hess.len());
        f
    }
}



#[inline]
pub(crate) fn check_lift_shapes(n: usize, grad: usize, hess: usize) {
    assert!(grad == n, "Scalar::lift: gradient length {grad} != number of inputs {n}");
    assert!(hess == 0 || hess == n * n, "Scalar::lift: Hessian length {hess} is neither 0 nor {n}²");
}

#[must_use]
pub fn sum<S: Scalar>(xs: &[S]) -> S {
    let mut acc = S::zero();
    for &x in xs {
        acc += x;
    }
    acc
}



#[must_use]
pub fn dot<S: Scalar>(a: &[S], b: &[S]) -> S {
    assert_eq!(a.len(), b.len(), "dot: length mismatch");
    let mut acc = S::zero();
    for (&x, &y) in a.iter().zip(b) {
        acc += x * y;
    }
    acc
}

#[must_use]
pub fn norm<S: Scalar>(x: &[S]) -> S {
    dot(x, x).sqrt()
}

#[inline]
#[must_use]
pub fn select<S: Scalar>(cond: bool, a: S, b: S) -> S {
    if cond { a } else { b }
}
