// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::{Dual, Scalar};

use super::record::{PhaseTransition, SolidMaterial, idx};
use crate::pchip::PropertyCurve;

pub fn affine<S: Scalar>(m: &SolidMaterial, previous: S, delta: S) -> S {
    let cp = m.values[idx::CP];
    let slope = m.slopes[4];
    ((previous - m.t_ref + delta * 0.5) * slope + cp) * delta
}

fn logcosh<S: Scalar>(v: S) -> S {
    v.logaddexp(-v) - std::f64::consts::LN_2
}

pub fn tanh_difference<S: Scalar>(a: S, b: S) -> S {
    if b.value().abs() < 1.0 {
        let t = a.tanh();
        let u = b.tanh();
        let denominator = t * u + 1.0;
        let e = (a.abs() * -2.0).exp();
        u * (e * 4.0 / (e + 1.0).powi(2)) / denominator
    } else {
        b.sign()
            * (b.abs() - std::f64::consts::LN_2 - logcosh(a) - logcosh(a + b)).exp()
            * (-(b.abs() * -2.0).exp_m1())
    }
}

pub fn phase<S: Scalar>(m: &SolidMaterial, p: &PhaseTransition, previous: S, delta: S) -> S {
    let width = 2.0 * p.width;
    affine(m, previous, delta)
        + tanh_difference((previous - p.temperature) / width, delta / width) * (0.5 * p.latent_heat)
}

fn raw_curve_increment<S: Scalar>(
    curve: &PropertyCurve,
    previous: S,
    delta: S,
    lower: Option<f64>,
    upper: Option<f64>,
) -> S {
    let mut result = S::zero();
    for i in 0..curve.knots.len() - 1 {
        let mut lo = curve.knots[i];
        let mut hi = curve.knots[i + 1];
        if let Some(l) = lower {
            lo = lo.max(l);
        }
        if let Some(u) = upper {
            hi = hi.min(u);
        }
        if hi <= lo {
            continue;
        }
        let a_lo = -previous + lo;
        let a_hi = -previous + hi;
        let start = S::zero().maximum(a_lo).minimum(a_hi);
        let end = delta.maximum(a_lo).minimum(a_hi);
        let d = end - start;
        let q = previous - curve.knots[i] + start;
        let (a, b, c, e) = (
            curve.coefficients[0][i],
            curve.coefficients[1][i],
            curve.coefficients[2][i],
            curve.coefficients[3][i],
        );
        let value = ((q * a + b) * q + c) * q + e;
        let slope = (q * (3.0 * a) + 2.0 * b) * q + c;
        result += d * (value + d * (slope * 0.5 + d * ((q * (3.0 * a) + b) / 3.0 + (d * a) / 4.0)));
    }
    result
}

pub fn table<S: Scalar>(cp: &PropertyCurve, previous: S, delta: S) -> S {
    let result = curve_increment(cp, previous, delta, None, None);
    let (k0, k1) = (cp.first_knot(), cp.last_knot());
    let (p, d) = (previous.value(), delta.value());
    let valid = p >= k0 && p <= k1 && d >= k0 - p && d <= k1 - p;
    if valid { result } else { S::from_f64(f64::NAN) }
}

pub fn outward_interval<S: Scalar>(
    curve: &PropertyCurve,
    boundary: f64,
    s0: S,
    ds: S,
    outward_derivative: f64,
) -> S {
    let qb = curve.value(boundary);
    let d = outward_derivative;
    #[allow(clippy::float_cmp)]
    if d == 0.0 {
        return ds * qb;
    }
    let floor = f64::EPSILON * qb;
    let l = (qb - floor) / d.abs();
    let decay = (s0 * -2.0 / l).exp();
    let tail = (decay * (ds * -2.0 / l).exp_m1() / (decay + 1.0)).ln_1p();
    ds * (qb + d * l) + tail * (d * l * l)
}

fn raw_numerical_table<S: Scalar>(
    cp: &PropertyCurve,
    t_min: f64,
    t_max: f64,
    previous: S,
    delta: S,
) -> S {
    let mut result = raw_curve_increment(cp, previous, delta, Some(t_min), Some(t_max));
    for (boundary, lower) in [(t_min, true), (t_max, false)] {
        let relative = -previous + boundary;
        let (start, end) = if lower {
            (S::zero().minimum(relative), delta.minimum(relative))
        } else {
            (S::zero().maximum(relative), delta.maximum(relative))
        };
        let length = end - start;
        let direction = if length.value() >= 0.0 { 1.0 } else { -1.0 };
        let a = start.minimum(end);
        let b = start.maximum(end);
        let s0 = if lower { relative - b } else { a - relative };
        let derivative = cp.derivative(boundary) * if lower { -1.0 } else { 1.0 };
        result += outward_interval(cp, boundary, s0, length * direction, derivative) * direction;
    }
    result
}

fn curve_derivatives(curve: &PropertyCurve, previous: f64, delta: f64) -> (f64, f64) {
    let i = curve
        .knots
        .partition_point(|&k| delta >= k - previous)
        .saturating_sub(1)
        .min(curve.knots.len() - 2);
    let q = (previous - curve.knots[i]) + delta;
    (
        (q * (3.0 * curve.coefficients[0][i]) + 2.0 * curve.coefficients[1][i]) * q
            + curve.coefficients[2][i],
        6.0 * curve.coefficients[0][i] * q + 2.0 * curve.coefficients[1][i],
    )
}

fn upper_derivatives(curve: &PropertyCurve, upper: f64) -> (f64, f64) {
    let i = curve
        .knots
        .partition_point(|&k| k < upper)
        .saturating_sub(1)
        .min(curve.knots.len() - 2);
    let q = upper - curve.knots[i];
    (
        (q * (3.0 * curve.coefficients[0][i]) + 2.0 * curve.coefficients[1][i]) * q
            + curve.coefficients[2][i],
        6.0 * curve.coefficients[0][i] * q + 2.0 * curve.coefficients[1][i],
    )
}

fn numerical_derivatives(
    curve: &PropertyCurve,
    lower: f64,
    upper: f64,
    previous: f64,
    delta: f64,
) -> (f64, f64) {
    let (boundary, direction) = if delta < lower - previous {
        (lower, -1.0)
    } else if delta > upper - previous {
        (upper, 1.0)
    } else {
        return if delta == upper - previous {
            upper_derivatives(curve, upper)
        } else {
            curve_derivatives(curve, previous, delta)
        };
    };
    let qb = curve.value(boundary);
    let slope = curve.derivative(boundary);
    if slope == 0.0 {
        return (0.0, 0.0);
    }
    let length = (qb - f64::EPSILON * qb) / slope.abs();
    let distance = ((previous - boundary) + delta) * direction;
    let tanh = (distance / length).tanh();
    let sech2 = 1.0 - tanh * tanh;
    (
        slope * sech2,
        -2.0 * slope * direction / length * tanh * sech2,
    )
}

fn at_offset(previous: f64, delta: f64, knot: f64) -> bool {
    previous == knot || delta == knot - previous
}

fn knot_case(
    curve: &PropertyCurve,
    previous: f64,
    delta: f64,
    bounds: Option<(f64, f64)>,
) -> (bool, bool, bool) {
    let mut affected = false;
    let mut start_break = false;
    let mut end_break = false;
    for i in 1..curve.knots.len() - 1 {
        let knot = curve.knots[i];
        if bounds.is_some_and(|(l, u)| knot <= l || knot >= u) || !at_offset(previous, delta, knot)
        {
            continue;
        }
        affected = true;
        let h = knot - curve.knots[i - 1];
        let left = 6.0 * curve.coefficients[0][i - 1] * h + 2.0 * curve.coefficients[1][i - 1];
        let discontinuous = left != 2.0 * curve.coefficients[1][i];
        start_break |= discontinuous && previous == knot;
        end_break |= discontinuous && delta == knot - previous;
    }
    if let Some((lower, upper)) = bounds {
        for boundary in [lower, upper] {
            if at_offset(previous, delta, boundary) {
                affected = true;

                let discontinuous = if boundary == upper {
                    upper_derivatives(curve, upper).1 != 0.0
                } else {
                    curve_derivatives(curve, boundary, 0.0).1 != 0.0
                };
                start_break |= discontinuous && previous == boundary;
                end_break |= discontinuous && delta == boundary - previous;
            }
        }
    }
    (affected, start_break, end_break)
}

fn attach_integral<S: Scalar>(
    result: S,
    previous: S,
    delta: S,
    first: Dual<2>,
    start: (f64, f64),
    end: (f64, f64),
    start_break: bool,
    end_break: bool,
) -> S {
    let h = [end.0 - start.0, end.0, end.0];
    S::chain2_3(
        previous,
        delta,
        result.value(),
        first.eps[0],
        first.eps[1],
        h[0],
        h[1],
        h[2],
        || {
            let c0 = if start_break { f64::NAN } else { start.1 };
            let c1 = if end_break { f64::NAN } else { end.1 };

            [if delta.value() == 0.0 { 0.0 } else { c1 - c0 }, c1, c1, c1]
        },
    )
}

pub fn curve_increment<S: Scalar>(
    curve: &PropertyCurve,
    previous: S,
    delta: S,
    lower: Option<f64>,
    upper: Option<f64>,
) -> S {
    let result = raw_curve_increment(curve, previous, delta, lower, upper);
    if lower.is_some() || upper.is_some() {
        return result;
    }
    let (p, d) = (previous.value(), delta.value());
    let (lo, hi) = (curve.first_knot(), curve.last_knot());
    if !(p > lo && p < hi && d > lo - p && d < hi - p) {
        return result;
    }
    let (affected, start_break, end_break) = knot_case(curve, p, d, None);
    if !affected {
        return result;
    }
    let first = raw_curve_increment(
        curve,
        Dual::<2>::variable(p, 0),
        Dual::<2>::variable(d, 1),
        None,
        None,
    );
    attach_integral(
        result,
        previous,
        delta,
        first,
        curve_derivatives(curve, p, 0.0),
        curve_derivatives(curve, p, d),
        start_break,
        end_break,
    )
}

pub fn numerical_table<S: Scalar>(
    curve: &PropertyCurve,
    lower: f64,
    upper: f64,
    previous: S,
    delta: S,
) -> S {
    let result = raw_numerical_table(curve, lower, upper, previous, delta);
    let (p, d) = (previous.value(), delta.value());
    let (affected, start_break, end_break) = knot_case(curve, p, d, Some((lower, upper)));
    if !affected {
        return result;
    }
    let first = raw_numerical_table(
        curve,
        lower,
        upper,
        Dual::<2>::variable(p, 0),
        Dual::<2>::variable(d, 1),
    );
    attach_integral(
        result,
        previous,
        delta,
        first,
        numerical_derivatives(curve, lower, upper, p, 0.0),
        numerical_derivatives(curve, lower, upper, p, d),
        start_break,
        end_break,
    )
}
