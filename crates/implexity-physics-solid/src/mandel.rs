// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
