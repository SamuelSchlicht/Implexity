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
