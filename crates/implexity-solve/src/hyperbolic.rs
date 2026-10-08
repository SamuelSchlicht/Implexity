// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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

