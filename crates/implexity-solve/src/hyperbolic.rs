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



use implexity_ad::Scalar;
use implexity_core::error::{CaeError, CaeResult};

fn check_eps(eps: f64) -> CaeResult<()> {
    if eps > 0.0 { Ok(()) } else { Err(CaeError::contract("eps must be positive")) }
}



pub fn smooth_abs<S: Scalar>(x: S, eps: f64) -> CaeResult<S> {
    check_eps(eps)?;
    Ok((x * x + eps * eps).sqrt())
}



pub fn smooth_max2<S: Scalar>(a: S, b: S, eps: f64) -> CaeResult<S> {
    Ok((a + b + smooth_abs(a - b, eps)?) * 0.5)
}



pub fn rusanov_flux<S: Scalar>(
    left: &[S],
    right: &[S],
    components: usize,
    flux: impl Fn(&[S]) -> Vec<S>,
    speed: impl Fn(&[S]) -> S,
    smoothing: f64,
) -> CaeResult<Vec<S>> {
    let k = components.max(1);
    if left.len() != right.len() || !left.len().is_multiple_of(k) {
        return Err(CaeError::contract("physical_flux must preserve state shape"));
    }
    let mut out = Vec::with_capacity(left.len());
    for (ul, ur) in left.chunks(k).zip(right.chunks(k)) {
        let fl = flux(ul);
        let fr = flux(ur);
        if fl.len() != k || fr.len() != k {
            return Err(CaeError::contract("physical_flux must preserve state shape"));
        }
        let a = smooth_max2(speed(ul).abs(), speed(ur).abs(), smoothing)?;
        for c in 0..k {
            out.push((fl[c] + fr[c]) * 0.5 - a * (ur[c] - ul[c]) * 0.5);
        }
    }
    Ok(out)
}

#[must_use]
pub fn smooth_shock_sensor<S: Scalar>(left: S, center: S, right: S, eps: f64) -> S {
    let second = (right - center * 2.0 + left).abs();
    let scale = (right - center).abs() + (center - left).abs() + eps;
    let raw = second / scale;
    raw / (raw + 1.0)
}



pub fn conservative_update<S: Scalar>(
    state: &[S],
    interface_flux: &[S],
    components: usize,
    dt: f64,
    measure: &[f64],
) -> CaeResult<Vec<S>> {
    let k = components.max(1);
    let n = state.len() / k;
    if interface_flux.len() != (n + 1) * k {
        return Err(CaeError::contract("interface_flux must contain N+1 interfaces for N cells"));
    }
    let vol: Vec<f64> = if measure.len() == 1 { vec![measure[0]; n] } else { measure.to_vec() };
    if vol.len() != n || vol.iter().any(|v| *v <= 0.0) {
        return Err(CaeError::contract("cell_measure must be positive scalar or length-N vector"));
    }
    let mut out = Vec::with_capacity(state.len());
    for i in 0..n {
        for c in 0..k {
            let div = interface_flux[(i + 1) * k + c] - interface_flux[i * k + c];
            out.push(state[i * k + c] - div * dt / vol[i]);
        }
    }
    Ok(out)
}



#[allow(clippy::too_many_arguments)]
pub fn conservation_error(
    before: &[f64],
    after: &[f64],
    components: usize,
    measure: &[f64],
    flux_left: &[f64],
    flux_right: &[f64],
    dt: f64,
) -> CaeResult<Vec<f64>> {
    let k = components.max(1);
    let n = before.len() / k;
    if after.len() != before.len() || flux_left.len() != k || flux_right.len() != k {
        return Err(CaeError::contract("conservation error arguments have inconsistent shapes"));
    }
    let vol: Vec<f64> = if measure.len() == 1 { vec![measure[0]; n] } else { measure.to_vec() };
    if vol.len() != n {
        return Err(CaeError::contract("cell_measure must be positive scalar or length-N vector"));
    }
    Ok((0..k)
        .map(|c| {
            let change: f64 = (0..n).map(|i| (after[i * k + c] - before[i * k + c]) * vol[i]).sum();
            change - (-dt * (flux_right[c] - flux_left[c]))
        })
        .collect())
}

