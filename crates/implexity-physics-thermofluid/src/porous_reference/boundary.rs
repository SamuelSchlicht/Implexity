// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use super::ad::{A, Shape};
use super::grid::flat;

fn ray_order(s: [usize; 3], a: usize, reverse: bool) -> Vec<usize> {
    let mut idx = Vec::with_capacity(s[0] * s[1] * s[2]);
    for i in 0..s[0] {
        for j in 0..s[1] {
            for k in 0..s[2] {
                let mut p = [i, j, k];
                if reverse {
                    p[a] = s[a] - 1 - p[a];
                }
                idx.push(flat(s, p[0], p[1], p[2]));
            }
        }
    }
    idx
}

pub fn attenuated_measure(rho: A<'_>, h: f64, axis: usize, side: usize, depth: f64) -> A<'_> {
    let s = rho.shape().as3();
    let tau = rho * (h / depth);
    let order = ray_order(s, axis, side == 1);
    let t = tau.gather(order.clone(), Shape::d3(s));
    let above = super::ops::cumsum(t, axis) - t;
    let mu = above.g_map2(t, |a, t| (-a).exp() * (-(-t).exp_m1()));
    mu.gather(order, Shape::d3(s))
}

pub fn ray_support(mask: &[f64], s: [usize; 3], axis: usize) -> Vec<f64> {
    let mut out = vec![0.0; mask.len()];
    for i in 0..s[0] {
        for j in 0..s[1] {
            for k in 0..s[2] {
                let mut m = f64::NEG_INFINITY;
                for c in 0..s[axis] {
                    let mut p = [i, j, k];
                    p[axis] = c;
                    m = m.max(mask[flat(s, p[0], p[1], p[2])]);
                }
                out[flat(s, i, j, k)] = m;
            }
        }
    }
    out
}

pub fn unscaled_resultant<'g>(total: A<'g>, measure: A<'g>) -> A<'g> {
    total * measure
}

pub fn measure_sum(measure: A<'_>) -> A<'_> { measure.sum() }

pub fn normalized_resultant<'g>(total: A<'g>, measure: A<'g>, sum: A<'g>, reference: A<'g>, floor: f64) -> A<'g> {
    let denom = measure.graph().mapn([sum, reference], move |[s, r]| (s * s + (r * floor) * (r * floor)).sqrt());
    total * measure / denom
}

pub fn absorbed_surface_power<'g>(rho: A<'g>, spacing: f64, penetration: f64, flux: f64, axis: usize) -> A<'g> {
    let tau = rho * (spacing / penetration);
    let rev = super::ops::flip(tau, axis);
    let cum = super::ops::flip(super::ops::cumsum(rev, axis), axis);
    let above = cum - tau;
    let absorbed = above.g_map2(tau, move |a, t| (-a).exp() * (-(-t).exp_m1()) * flux);
    absorbed * (spacing * spacing)
}
