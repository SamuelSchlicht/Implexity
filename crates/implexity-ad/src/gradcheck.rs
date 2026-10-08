// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use crate::error::AdError;

#[must_use]
pub fn default_relative_step() -> f64 {
    f64::EPSILON.cbrt()
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProbeReport {
    pub index: usize,
    pub x: f64,
    pub h: f64,
    pub value: f64,
    pub ad: f64,
    pub fd: f64,
    pub rel_err: f64,
    pub comparison: &'static str,
}



pub fn fd_vs_ad_probe<F>(
    mut f: F,
    x: &[f64],
    index: usize,
    value: f64,
    ad: f64,
    step: Option<f64>,
) -> Result<ProbeReport, AdError>
where
    F: FnMut(&[f64]) -> Result<f64, AdError>,
{
    if let Some(s) = step
        && !(s.is_finite() && s > 0.0)
    {
        return Err(AdError::Invalid("gradient-check step must be finite and positive".into()));
    }
    if index >= x.len() {
        return Err(AdError::Invalid(format!("index {index} out of range for size {}", x.len())));
    }
    if !x.iter().all(|v| v.is_finite()) {
        return Err(AdError::NonFinite("gradient-check design must be finite".into()));
    }
    let step = step.unwrap_or_else(default_relative_step);
    let xi = x[index];
    let h = step * xi.abs().max(1.0);
    if !(h.is_finite() && h > 0.0) {
        return Err(AdError::Invalid("gradient-check perturbation must be finite and positive".into()));
    }
    if !value.is_finite() || !ad.is_finite() {
        return Err(AdError::NonFinite(
            "nonfinite base loss or automatic derivative cannot pass a gradient check".into(),
        ));
    }
    let mut shifted = |sign: f64| -> Result<f64, AdError> {
        let mut xs = x.to_vec();
        xs[index] += sign * h;
        let moved = xs[index];
        if !moved.is_finite() || sign * (moved - xi) <= 0.0 {
            return Err(AdError::Invalid(
                "gradient-check step is not representable in the design dtype".into(),
            ));
        }
        let v = f(&xs)?;
        if !v.is_finite() {
            return Err(AdError::NonFinite("nonfinite perturbed loss cannot pass a gradient check".into()));
        }
        Ok(v)
    };
    let plus = shifted(1.0)?;
    let minus = shifted(-1.0)?;
    let fd = (plus - minus) / (2.0 * h);
    if !fd.is_finite() {
        return Err(AdError::NonFinite(
            "nonfinite finite-difference derivative cannot pass a gradient check".into(),
        ));
    }
    let scale = ad.abs().max(fd.abs());
    let (rel_err, comparison) = if scale > 0.0 {
        ((ad / scale - fd / scale).abs(), "nonzero_relative")
    } else {
        (0.0, "zero_direction")
    };
    Ok(ProbeReport { index, x: xi, h, value, ad, fd, rel_err, comparison })
}

#[derive(Clone, Debug, PartialEq)]
pub struct IntervalEstimate {
    pub central: f64,
    pub forward: f64,
    pub probe: f64,
    pub curvature: f64,
    pub third_derivative: f64,
    pub evaluations: usize,
}

fn eval_at<F>(f: &mut F, x: &[f64], i: usize, dx: f64, count: &mut usize) -> Result<f64, AdError>
where
    F: FnMut(&[f64]) -> Result<f64, AdError>,
{
    let mut xs = x.to_vec();
    xs[i] += dx;
    *count += 1;
    let v = f(&xs)?;
    if v.is_finite() {
        Ok(v)
    } else {
        Err(AdError::NonFinite(format!("function is not finite at offset {dx:e} of coordinate {i}")))
    }
}




pub fn gmw_central_step<F>(
    mut f: F,
    x: &[f64],
    index: usize,
    absolute_error: Option<f64>,
) -> Result<IntervalEstimate, AdError>
where
    F: FnMut(&[f64]) -> Result<f64, AdError>,
{
    if index >= x.len() {
        return Err(AdError::Invalid(format!("index {index} out of range for size {}", x.len())));
    }
    let mut count = 0usize;
    let f0 = eval_at(&mut f, x, index, 0.0, &mut count)?;
    let ea = absolute_error.unwrap_or(f64::EPSILON * (1.0 + f0.abs()));
    if !(ea.is_finite() && ea > 0.0) {
        return Err(AdError::Invalid("absolute error bound must be finite and positive".into()));
    }
    let xi = x[index];
    let second = |f: &mut F, h: f64, count: &mut usize| -> Result<f64, AdError> {
        let p = eval_at(f, x, index, h, count)?;
        let m = eval_at(f, x, index, -h, count)?;
        Ok((p - 2.0 * f0 + m) / (h * h))
    };
    let cancel = |h: f64, phi: f64| if phi == 0.0 { f64::INFINITY } else { 4.0 * ea / (h * h * phi.abs()) };
    let mut h = 2.0 * (1.0 + xi.abs()) * (ea / (1.0 + f0.abs())).sqrt();
    let mut phi = second(&mut f, h, &mut count)?;
    let mut c = cancel(h, phi);
    let max_changes = 6;
    let (probe, curvature) = if (0.001..=0.1).contains(&c) {
        (h, phi)
    } else if c > 0.1 {

        let mut accepted = (h, phi);
        for _ in 0..max_changes {
            h *= 10.0;
            phi = second(&mut f, h, &mut count)?;
            c = cancel(h, phi);
            accepted = (h, phi);
            if c <= 0.1 {
                break;
            }
        }
        accepted
    } else {

        let mut accepted = (h, phi);
        for _ in 0..max_changes {
            let h_new = h / 10.0;
            let phi_new = second(&mut f, h_new, &mut count)?;
            let c_new = cancel(h_new, phi_new);
            if c_new > 0.1 || (phi_new - phi).abs() > 0.5 * phi_new.abs() {
                break;
            }
            h = h_new;
            phi = phi_new;
            accepted = (h, phi);
            if c_new >= 0.001 {
                break;
            }
        }
        accepted
    };
    let forward = if curvature == 0.0 { probe } else { 2.0 * (ea / curvature.abs()).sqrt() };
    let p1 = eval_at(&mut f, x, index, probe, &mut count)?;
    let m1 = eval_at(&mut f, x, index, -probe, &mut count)?;
    let p2 = eval_at(&mut f, x, index, 2.0 * probe, &mut count)?;
    let m2 = eval_at(&mut f, x, index, -2.0 * probe, &mut count)?;
    let third = (p2 - 2.0 * p1 + 2.0 * m1 - m2) / (2.0 * probe * probe * probe);
    let central = if third == 0.0 || !third.is_finite() { probe } else { (3.0 * ea / third.abs()).cbrt() };
    Ok(IntervalEstimate { central, forward, probe, curvature, third_derivative: third, evaluations: count })
}



pub fn central_difference<F>(mut f: F, x: &[f64], index: usize, h: f64) -> Result<f64, AdError>
where
    F: FnMut(&[f64]) -> Result<f64, AdError>,
{
    if index >= x.len() {
        return Err(AdError::Invalid(format!("index {index} out of range for size {}", x.len())));
    }
    if !(h.is_finite() && h > 0.0) {
        return Err(AdError::Invalid("finite-difference step must be finite and positive".into()));
    }
    let mut count = 0;
    let p = eval_at(&mut f, x, index, h, &mut count)?;
    let m = eval_at(&mut f, x, index, -h, &mut count)?;
    Ok((p - m) / (2.0 * h))
}

#[derive(Clone, Debug, PartialEq)]
pub struct TaylorReport {
    pub steps: Vec<f64>,
    pub remainders: Vec<f64>,
    pub orders: Vec<f64>,
    pub consecutive_second_order: usize,
    pub passed: bool,
}




pub fn taylor_test<F>(
    mut f: F,
    x: &[f64],
    gradient: &[f64],
    direction: &[f64],
    h0: f64,
    halvings: usize,
    required: usize,
) -> Result<TaylorReport, AdError>
where
    F: FnMut(&[f64]) -> Result<f64, AdError>,
{
    if gradient.len() != x.len() || direction.len() != x.len() {
        return Err(AdError::Shape("taylor_test: gradient/direction length mismatch".into()));
    }
    if !(h0.is_finite() && h0 > 0.0) {
        return Err(AdError::Invalid("taylor_test: h0 must be finite and positive".into()));
    }
    let j0 = f(x)?;
    let slope: f64 = gradient.iter().zip(direction).map(|(g, d)| g * d).sum();
    let floor = 1e3 * f64::EPSILON * j0.abs().max(1.0);
    let mut steps = Vec::with_capacity(halvings + 1);
    let mut remainders = Vec::with_capacity(halvings + 1);
    let mut h = h0;
    for _ in 0..=halvings {
        let xs: Vec<f64> = x.iter().zip(direction).map(|(a, d)| a + h * d).collect();
        let jh = f(&xs)?;
        steps.push(h);
        remainders.push((jh - j0 - h * slope).abs());
        h *= 0.5;
    }
    let orders: Vec<f64> = remainders.windows(2).map(|w| (w[0] / w[1]).log2()).collect();

    let last = orders.iter().enumerate().rev().find(|&(k, _)| remainders[k + 1] > floor).map(|(k, _)| k);
    let mut best = 0;
    if let Some(last) = last {
        for k in (0..=last).rev() {
            if remainders[k + 1] > floor && (1.8..=2.2).contains(&orders[k]) {
                best += 1;
            } else {
                break;
            }
        }
    }
    Ok(TaylorReport { steps, remainders, orders, consecutive_second_order: best, passed: best >= required })
}
