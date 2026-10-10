// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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
