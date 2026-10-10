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

use super::laws::Prony;
use implexity_core::{CaeError, CaeResult};
fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}
pub fn fit_storage_prony(ratio: f64, f_target_hz: f64, f_quasi_static_hz: f64) -> CaeResult<Prony> {
    if !(ratio.is_finite()
        && ratio > 1.0
        && f_quasi_static_hz > 0.0
        && f_target_hz > 10.0 * f_quasi_static_hz)
    {
        return contract(
            "storage_prony: a storage ratio above 1 and a target frequency at least ten times the quasi-static one",
        );
    }
    let two_pi = 2.0 * std::f64::consts::PI;
    let (lq, lt) = (f_quasi_static_hz.ln(), f_target_hz.ln());
    let tau: Vec<f64> =
        [1.0 / 6.0, 0.5, 5.0 / 6.0].iter().map(|q| 1.0 / (two_pi * (lq + q * (lt - lq)).exp())).collect();

    let samples: Vec<f64> = (0..=80).map(|i| (lq - 1.0) + (lt - lq + 2.0) * f64::from(i) / 80.0).collect();
    let mut a = Vec::new();
    let mut b = Vec::new();
    for lf in samples {
        let w = ((lf - lq) / (lt - lq)).clamp(0.0, 1.0);
        let target = ratio.powf(w) - 1.0;
        let omega = two_pi * lf.exp();
        a.push(tau.iter().map(|t| (omega * t).powi(2) / (1.0 + (omega * t).powi(2))).collect::<Vec<_>>());
        b.push(target);
    }
    let mut beta = implexity_linalg::nnls::nonnegative_least_squares(&a, &b)
        .map_err(|e| CaeError::contract(e.to_string()))?;
    let p = Prony { beta: beta.clone(), tau: tau.clone() };
    let have = p.storage_ratio(two_pi * f_target_hz) - 1.0;
    if have <= 0.0 {
        return Err(CaeError::convergence("storage_prony: the fit carries no branch"));
    }
    for v in &mut beta {
        *v *= (ratio - 1.0) / have;
    }
    let keep: Vec<usize> = (0..beta.len()).filter(|i| beta[*i] > 1e-9 * (ratio - 1.0)).collect();
    Ok(Prony { beta: keep.iter().map(|i| beta[*i]).collect(), tau: keep.iter().map(|i| tau[*i]).collect() })
}

pub fn fit_young_storage_prony(
    young: f64,
    nu: f64,
    target: f64,
    f_target: f64,
    f_low: f64,
) -> CaeResult<Prony> {
    let k = super::reporting::bulk_modulus(young, nu);
    if !(young.is_finite()
        && young > 0.0
        && nu.is_finite()
        && nu > -1.0
        && nu < 0.5
        && target.is_finite()
        && target > young
        && target < 9.0 * k)
    {
        return contract("Young storage target must lie above equilibrium and below9K");
    }
    let mut p = fit_storage_prony(target / young, f_target, f_low)?;
    let beta = p.beta.clone();
    let value = |scale: f64| {
        let branches: Vec<_> = beta.iter().zip(&p.tau).map(|(&b, &t)| (scale * b, t)).collect();
        super::reporting::complex_young_modulus(young, nu, &branches, f_target).0
    };
    let (mut lo, mut hi) = (0.0, 1.0);
    while value(hi) < target {
        hi *= 2.0;
        if !hi.is_finite() {
            return Err(CaeError::convergence("Young storage fit bracket failed"));
        }
    }
    for _ in 0..100 {
        let mid = 0.5 * (lo + hi);
        if value(mid) < target { lo = mid } else { hi = mid }
    }
    for b in &mut p.beta {
        *b *= 0.5 * (lo + hi);
    }
    Ok(p)
}

#[derive(Clone, Copy, Debug)]
pub struct LogScaleOptions {
    pub target_hz: f64,
    pub mode: usize,
    pub tolerance: f64,
    pub max_iterations: usize,
}
#[derive(Clone, Debug)]
pub struct LogScaleStep {
    pub scale: f64,
    pub frequencies_hz: Vec<f64>,
    pub d_frequency_d_ln_scale: f64,
    pub residual: f64,
}
pub fn fit_modal_log_scale(
    options: LogScaleOptions,
    mut evaluate: impl FnMut(f64, f64) -> CaeResult<(Vec<f64>, f64)>,
) -> CaeResult<Vec<LogScaleStep>> {
    if !(options.target_hz.is_finite() && options.target_hz > 0.0 && options.tolerance > 0.0) {
        return contract("modal calibration target and tolerance must be positive");
    }
    let mut scale = 1.0;
    let mut factor = 1.0;
    let mut steps = Vec::new();
    for _ in 0..options.max_iterations.max(1) {
        let (f, df) = evaluate(scale, factor)?;
        let fk = *f.get(options.mode).ok_or_else(|| CaeError::contract("calibration mode is unavailable"))?;
        let residual = fk.ln() - options.target_hz.ln();
        steps.push(LogScaleStep { scale, frequencies_hz: f, d_frequency_d_ln_scale: df, residual });
        if residual.abs() <= options.tolerance {
            return Ok(steps);
        }
        let slope = df / fk;
        if !(slope.is_finite() && slope > 0.0) {
            return Err(CaeError::convergence(format!(
                "matched frequency does not rise with modulus: log slope{slope:.3e}"
            )));
        }
        factor = (-residual / slope).clamp(-4.0_f64.ln(), 4.0_f64.ln()).exp();
        scale *= factor;
    }
    Err(CaeError::convergence(format!(
        "calibration did not converge in{}iterations (last residual{:.3e})",
        options.max_iterations,
        steps.last().map_or(f64::NAN, |s| s.residual)
    )))
}

pub fn upward_crossing_frequency(samples: &[f64], dt: f64) -> CaeResult<f64> {
    let crossings: Vec<f64> = samples.windows(2).enumerate()
        .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
        .map(|(k, w)| (k as f64 + 1.0 + w[0] / (w[0] - w[1])) * dt).collect();
    if crossings.len() < 2 {
        return Err(CaeError::convergence("free vibration: fewer than two upward crossings"));
    }
    let period = (crossings[crossings.len()-1] - crossings[0]) / (crossings.len()-1) as f64;
    Ok(1.0 / period)
}

pub fn fit_proportional_material_response(nominal_material: f64, target: f64, tolerance: f64,
    mut evaluate: impl FnMut(f64) -> CaeResult<f64>) -> CaeResult<(f64, f64, f64)> {
    let nominal = evaluate(nominal_material)?;
    let material = nominal_material * target / nominal;
    let verified = evaluate(material)?;
    let error = (verified / target - 1.0).abs();
    if error.is_nan() || error > tolerance {
        return Err(CaeError::convergence(format!(
            "response calibration: verification deviates by {error:.3e} from the target")));
    }
    Ok((nominal, material, verified))
}
