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

pub type Mandel<S> = [S; 6];

pub const SQRT2: f64 = std::f64::consts::SQRT_2;

pub const IDENTITY: [f64; 6] = [1.0, 1.0, 1.0, 0.0, 0.0, 0.0];

pub fn trace<S: Scalar>(a: &Mandel<S>) -> S {
    a[0] + a[1] + a[2]
}

pub fn dev<S: Scalar>(a: &Mandel<S>) -> Mandel<S> {
    let m = trace(a) / 3.0;
    [a[0] - m, a[1] - m, a[2] - m, a[3], a[4], a[5]]
}

pub fn dot<S: Scalar>(a: &Mandel<S>, b: &Mandel<S>) -> S {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3] + a[4] * b[4] + a[5] * b[5]
}

pub fn equivalent<S: Scalar>(a: &Mandel<S>) -> S {
    let s = dev(a);
    (dot(&s, &s) * 1.5 + 1e-24).sqrt()
}

pub fn add<S: Scalar>(a: &Mandel<S>, b: &Mandel<S>) -> Mandel<S> {
    std::array::from_fn(|i| a[i] + b[i])
}

pub fn sub<S: Scalar>(a: &Mandel<S>, b: &Mandel<S>) -> Mandel<S> {
    std::array::from_fn(|i| a[i] - b[i])
}

pub fn scale<S: Scalar>(a: &Mandel<S>, c: S) -> Mandel<S> {
    std::array::from_fn(|i| c * a[i])
}

pub fn from_slice<S: Scalar>(a: &[S]) -> Mandel<S> {
    std::array::from_fn(|i| a[i])
}

#[must_use]
pub fn zeros<S: Scalar>() -> Mandel<S> {
    [S::zero(); 6]
}

pub fn strain<S: Scalar>(u: &[[S; 3]; 4], grad: &[[S; 3]; 4]) -> Mandel<S> {
    let mut gu = [[S::zero(); 3]; 3];
    for (a, row) in gu.iter_mut().enumerate() {
        for (b, entry) in row.iter_mut().enumerate() {
            let mut acc = S::zero();
            for i in 0..4 {
                acc += u[i][a] * grad[i][b];
            }
            *entry = acc;
        }
    }
    let e = |a: usize, b: usize| (gu[a][b] + gu[b][a]) * 0.5;
    [e(0, 0), e(1, 1), e(2, 2), e(1, 2) * SQRT2, e(0, 2) * SQRT2, e(0, 1) * SQRT2]
}

pub fn tensor<S: Scalar>(s: &Mandel<S>) -> [[S; 3]; 3] {
    let r = |x: S| x / SQRT2;
    [[s[0], r(s[5]), r(s[4])], [r(s[5]), s[1], r(s[3])], [r(s[4]), r(s[3]), s[2]]]
}

pub fn nodal_forces<S: Scalar>(stress: &Mandel<S>, grad: &[[S; 3]; 4], volume: S) -> [[S; 3]; 4] {
    let t = tensor(stress);
    std::array::from_fn(|i| {
        std::array::from_fn(|a| {
            let mut acc = S::zero();
            for b in 0..3 {
                acc += t[a][b] * grad[i][b];
            }
            volume * acc
        })
    })
}

pub fn hooke<S: Scalar>(e_mod: S, nu: S, ee: &Mandel<S>) -> Mandel<S> {
    let g = e_mod / ((nu + 1.0) * 2.0);
    let k = e_mod / ((-(nu * 2.0) + 1.0) * 3.0);
    let d = dev(ee);
    let tr = trace(ee);
    std::array::from_fn(|i| g * 2.0 * d[i] + k * tr * IDENTITY[i])
}
