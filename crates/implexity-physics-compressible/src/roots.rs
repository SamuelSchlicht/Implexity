// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::{Dual, HyperDual, Jet3, Scalar};

#[must_use]
pub fn values<S: Scalar>(params: &[S]) -> Vec<f64> {
    params.iter().map(Scalar::value).collect()
}

#[must_use]
pub fn slope<R: Residual>(r: &R, x: f64, p: &[f64]) -> f64 {
    let pd: Vec<Dual<1>> = p.iter().map(|v| Dual::constant(*v)).collect();
    r.eval(Dual::<1>::variable(x, 0), &pd).eps[0]
}

#[must_use]
pub fn attach<S: Scalar, R: Residual>(r: &R, value: f64, at: f64, params: &[S]) -> S {
    let pv = values(params);
    let fx = slope(r, at, &pv);
    let f = r.eval(S::from_f64(at), params);
    let delta = f - S::from_f64(f.value());
    let first = S::from_f64(value) - delta / fx;
    let n = params.len();
    let constants: Vec<HyperDual> = pv.iter().copied().map(HyperDual::constant).collect();
    let fxx = r.eval(HyperDual::new(at, 1.0, 1.0, 0.0), &constants).e12;
    let mut fp = vec![0.0; n];
    let mut fxp = vec![0.0; n];
    for i in 0..n {
        let mut p = constants.clone();
        p[i] = HyperDual::new(pv[i], 0.0, 1.0, 0.0);
        let partial = r.eval(HyperDual::new(at, 1.0, 0.0, 0.0), &p);
        fp[i] = partial.e2;
        fxp[i] = partial.e12;
    }
    let g: Vec<f64> = fp.iter().map(|p| -p / fx).collect();
    let mut correction = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..n {
            correction[i * n + j] = -(fxp[i] * g[j] + fxp[j] * g[i] + fxx * g[i] * g[j]) / fx;
        }
    }

    first + S::lift3(0.0_f64.copysign(first.value()), params, &vec![0.0; n], &correction, |a, b| {
        let mut h = correction.clone();
        for i in 0..n {
            for j in 0..n {
                let p: Vec<HyperDual> = pv.iter().enumerate().map(|(k, v)| {
                    HyperDual::new(*v, if k == i { 1.0 } else { 0.0 }, if k == j { 1.0 } else { 0.0 }, 0.0)
                }).collect();
                h[i * n + j] -= r.eval(HyperDual::constant(at), &p).e12 / fx;
            }
        }
        let ga: f64 = g.iter().zip(a).map(|(g, a)| g * a).sum();
        let gb: f64 = g.iter().zip(b).map(|(g, b)| g * b).sum();
        let h_ab: f64 = (0..n).map(|i| a[i] * (0..n).map(|j| h[i * n + j] * b[j]).sum::<f64>()).sum();
        let fxa = fxx * ga + fxp.iter().zip(a).map(|(p, a)| p * a).sum::<f64>();
        let fxb = fxx * gb + fxp.iter().zip(b).map(|(p, b)| p * b).sum::<f64>();
        (0..n).map(|k| {
            let seed = Jet3::<1>::variable(0.0, 0, 0.0, 0.0);
            let p: Vec<Jet3<1>> = pv.iter().enumerate().map(|(i, v)| {
                let q = Jet3::directed(*v, a[i], b[i]);
                if i == k { q + seed } else { q }
            }).collect();
            let full = r.eval(Jet3::directed(at, ga, gb) + seed * g[k], &p);
            let fixed = r.eval(Jet3::constant(at), &p);
            let h_ka: f64 = (0..n).map(|i| h[k * n + i] * a[i]).sum();
            let h_kb: f64 = (0..n).map(|i| h[k * n + i] * b[i]).sum();
            -(full.third()[0] - fixed.third()[0] + h_ab * (fxx * g[k] + fxp[k]) + h_ka * fxb + h_kb * fxa) / fx
        }).collect()
    })
}

#[must_use]
pub fn temperature_root<S: Scalar, R: Residual>(r: &R, first: S, second: S, params: &[S]) -> S {
    let pv = values(params);
    let (a, b) = (first.value(), second.value());
    let (lower, upper) = if a.is_nan() || b.is_nan() {
        (f64::NAN, f64::NAN)
    } else if a <= b {
        (a, b)
    } else {
        (b, a)
    };
    let f = |x: f64| r.eval(x, &pv);
    let initial = f(lower);
    let (mut lo, mut hi) = (lower, upper);
    for _ in 0..64 {
        let mid = 0.5 * (lo + hi);
        if f(mid) * initial > 0.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let root = 0.5 * (lo + hi);
    attach(r, root, root, params)
}

#[must_use]
pub fn bracketed_newton_root<S: Scalar, R: Residual>(
    r: &R,
    lo: f64,
    hi: f64,
    newton_steps: usize,
    params: &[S],
) -> S {
    let pv = values(params);
    let f = |x: f64| r.eval(x, &pv);
    let initial_sign = sign(f(lo));
    let (mut a, mut b) = (lo, hi);
    for _ in 0..64 {
        let mid = 0.5 * (a + b);
        if f(mid) * initial_sign > 0.0 {
            a = mid;
        } else {
            b = mid;
        }
    }
    let mut root = 0.5 * (a + b);
    let mut last_input = root;
    for _ in 0..newton_steps {
        last_input = root;
        root -= f(root) / slope(r, root, &pv);
    }
    attach(r, root, last_input, params)
}

#[must_use]
pub fn sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else if x == 0.0 {
        0.0
    } else {
        f64::NAN
    }
}

pub trait Residual {
    fn eval<T: Scalar>(&self, x: T, p: &[T]) -> T;
}
