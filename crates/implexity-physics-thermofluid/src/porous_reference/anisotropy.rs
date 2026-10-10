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


use implexity_ad::{Dual, Scalar};
use serde_json::{Map, Value, json};
use super::ad::A;
const TWO_PI: f64 = 2.0 * std::f64::consts::PI;

type Sym<S> = [S; 6];

fn sym_det<S: Scalar>(m: &Sym<S>) -> S {
    let [a, b, c, e, f, i] = *m;
    a * (e * i - f * f) - b * (b * i - f * c) + c * (b * f - e * c)
}

fn sym_inv<S: Scalar>(m: &Sym<S>) -> Sym<S> {
    let [a, b, c, e, f, i] = *m;
    let a11 = e * i - f * f;
    let a12 = c * f - b * i;
    let a13 = b * f - c * e;
    let a22 = a * i - c * c;
    let a23 = b * c - a * f;
    let a33 = a * e - b * b;
    let det = a * a11 + b * a12 + c * a13;
    let dd = if det.value().abs() > 1e-300 { det } else { S::from_f64(1e-300) };
    let inv = S::from_f64(1.0) / dd;
    [a11 * inv, a12 * inv, a13 * inv, a22 * inv, a23 * inv, a33 * inv]
}

pub fn period_tensor<S: Scalar>(j: &[[S; 3]; 3], floor_rel: f64) -> Sym<S> {
    let gm = |i: usize, k: usize| j[0][i] * j[0][k] + j[1][i] * j[1][k] + j[2][i] * j[2][k];
    let mut gg = [gm(0, 0), gm(0, 1), gm(0, 2), gm(1, 1), gm(1, 2), gm(2, 2)];
    let tr = (gg[0] + gg[3] + gg[5]) / 3.0;
    gg[0] += tr * floor_rel;
    gg[3] += tr * floor_rel;
    gg[5] += tr * floor_rel;
    let inv = sym_inv(&gg);
    inv.map(|v| v * (TWO_PI * TWO_PI))
}

pub fn unit_shape<S: Scalar>(m: &Sym<S>) -> Sym<S> {
    let d = sym_det(m).max_f64(1e-300).powf(1.0 / 3.0);
    m.map(|v| v / d)
}

pub fn mean_period<S: Scalar>(m: &Sym<S>) -> S {
    sym_det(m).max_f64(1e-300).powf(1.0 / 6.0)
}

fn sym_sqrt<S: Scalar>(s: &Sym<S>, iters: usize) -> Sym<S> {
    let mut y = *s;
    let mut z = [S::one(), S::zero(), S::zero(), S::one(), S::zero(), S::one()];
    for _ in 0..iters {
        let zi = sym_inv(&z);
        let yi = sym_inv(&y);
        let yn: Sym<S> = std::array::from_fn(|k| (y[k] + zi[k]) * 0.5);
        let zn: Sym<S> = std::array::from_fn(|k| (z[k] + yi[k]) * 0.5);
        y = yn;
        z = zn;
    }
    y
}

pub fn shape_power<S: Scalar>(s: &Sym<S>, q: f64, form: &str) -> Sym<S> {
    let eye = [S::one(), S::zero(), S::zero(), S::one(), S::zero(), S::one()];
    if form == "identity" || q == 0.0 {
        return eye;
    }
    if form == "sqrt" {
        let r = sym_sqrt(s, 7);
        if (q - 0.5).abs() == 0.0 {
            return r;
        }
        let t = 2.0 * q;
        return std::array::from_fn(|k| eye[k] * (1.0 - t) + r[k] * t);
    }
    std::array::from_fn(|k| eye[k] * (1.0 - q) + s[k] * q)
}

#[must_use]
pub fn anisotropy_ratio(m: &Sym<f64>) -> f64 {
    let ev = sym_eigvals(m);
    (ev[2].max(0.0) / ev[0].max(1e-300)).sqrt()
}

#[must_use]
pub fn sym_eigvals(m: &Sym<f64>) -> [f64; 3] {
    let [a, b, c, e, f, i] = *m;
    let p1 = b * b + c * c + f * f;
    if p1 == 0.0 {
        let mut v = [a, e, i];
        v.sort_by(f64::total_cmp);
        return v;
    }
    let q = (a + e + i) / 3.0;
    let p2 = (a - q).powi(2) + (e - q).powi(2) + (i - q).powi(2) + 2.0 * p1;
    let p = (p2 / 6.0).sqrt();
    let bm = [(a - q) / p, b / p, c / p, (e - q) / p, f / p, (i - q) / p];
    let r = (sym_det(&bm) / 2.0).clamp(-1.0, 1.0);
    let phi = r.acos() / 3.0;
    let e1 = q + 2.0 * p * phi.cos();
    let e3 = q + 2.0 * p * (phi + 2.0 * std::f64::consts::PI / 3.0).cos();
    let e2 = 3.0 * q - e1 - e3;
    let mut v = [e1, e2, e3];
    v.sort_by(f64::total_cmp);
    v
}

#[must_use]
pub fn offdiagonal_fraction(k: &Sym<f64>) -> f64 {
    let fro2 = k[0] * k[0] + k[3] * k[3] + k[5] * k[5] + 2.0 * (k[1] * k[1] + k[2] * k[2] + k[4] * k[4]);
    let dia2 = k[0] * k[0] + k[3] * k[3] + k[5] * k[5];
    ((fro2 - dia2).max(0.0) / fro2.max(1e-300)).sqrt()
}

#[must_use]
pub fn frame_shape<'g>(jac: &[[A<'g>; 3]; 3], q: f64, family_form: &str) -> ([A<'g>; 3], Map<String, Value>) {
    let g = jac[0][0].graph();
    let ins =
        [jac[0][0], jac[0][1], jac[0][2], jac[1][0], jac[1][1], jac[1][2], jac[2][0], jac[2][1], jac[2][2]];
    let form = family_form.to_string();
    let diag = g.mapn_multi(ins, move |x: [Dual<9>; 9]| {
        let j = [[x[0], x[1], x[2]], [x[3], x[4], x[5]], [x[6], x[7], x[8]]];
        let m = period_tensor(&j, 1e-3);
        let p = shape_power(&unit_shape(&m), q, &form);
        [p[0], p[3], p[5]]
    });
    let jv: Vec<Vec<f64>> = ins.iter().map(A::val).collect();
    let n = jv[0].len();
    let mut ktensor = Vec::with_capacity(n);
    let mut offd = Vec::with_capacity(n);
    let mut mt = Vec::with_capacity(n);
    let mut ratio = Vec::with_capacity(n);
    let mut mean = Vec::with_capacity(n);
    #[allow(clippy::needless_range_loop)]
    for c in 0..n {
        let j =
            [[jv[0][c], jv[1][c], jv[2][c]], [jv[3][c], jv[4][c], jv[5][c]], [jv[6][c], jv[7][c], jv[8][c]]];
        let m = period_tensor(&j, 1e-3);
        let p = shape_power(&unit_shape(&m), q, family_form);
        ktensor.push(json!([[p[0], p[1], p[2]], [p[1], p[3], p[4]], [p[2], p[4], p[5]]]));
        offd.push(offdiagonal_fraction(&p));
        mt.push(json!([[m[0], m[1], m[2]], [m[1], m[3], m[4]], [m[2], m[4], m[5]]]));
        ratio.push(anisotropy_ratio(&m));
        mean.push(mean_period(&m));
    }
    let mut rep = Map::new();
    rep.insert("K_shape_tensor".into(), Value::Array(ktensor));
    rep.insert("K_offdiag_fraction".into(), json!(offd));
    rep.insert("period_tensor".into(), Value::Array(mt));
    rep.insert("period_ratio".into(), json!(ratio));
    rep.insert("period_mean_m".into(), json!(mean));
    rep.insert("aniso_q".into(), json!(q));
    (diag, rep)
}

