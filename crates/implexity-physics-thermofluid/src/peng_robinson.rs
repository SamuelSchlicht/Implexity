// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;

pub const GAS_CONSTANT_J_MOL_K: f64 = 8.314_462_618_153_24;

pub const SQRT_TWO: f64 = std::f64::consts::SQRT_2;

#[must_use]
pub fn cubic_coefficients<S: Scalar>(attraction: S, covolume: S) -> (S, S, S) {
    let b = covolume;
    (b - 1.0, attraction - b * b * 3.0 - b * 2.0, -attraction * b + b * b + b * b * b)
}

#[must_use]
pub fn cubic_residual<S: Scalar>(z: S, attraction: S, covolume: S) -> S {
    let (c2, c1, c0) = cubic_coefficients(attraction, covolume);
    ((z + c2) * z + c1) * z + c0
}

#[must_use]
pub fn cubic_root_derivative<S: Scalar>(z: S, attraction: S, covolume: S) -> S {
    let (c2, c1, _) = cubic_coefficients(attraction, covolume);
    (z * 3.0 + c2 * 2.0) * z + c1
}

#[must_use]
pub fn root_metrics(z: f64, attraction: f64, covolume: f64) -> (f64, f64, f64) {
    let (c2, c1, c0) = cubic_coefficients(attraction, covolume);
    let residual_scale = 1.0 + (z.powi(3)).abs() + (c2 * z * z).abs() + (c1 * z).abs() + c0.abs();
    let jacobian_scale = 1.0 + 3.0 * z * z + (2.0 * c2 * z).abs() + c1.abs();
    let discriminant =
        c2 * c2 * c1 * c1 - 4.0 * c1.powi(3) - 4.0 * c2.powi(3) * c0 - 27.0 * c0 * c0 + 18.0 * c2 * c1 * c0;
    (
        cubic_residual(z, attraction, covolume).abs() / residual_scale,
        cubic_root_derivative(z, attraction, covolume).abs() / jacobian_scale,
        discriminant,
    )
}

#[must_use]
pub fn largest_real_root_value(attraction: f64, covolume: f64) -> f64 {
    let (c2, c1, c0) = cubic_coefficients(attraction, covolume);
    let p = c1 - c2 * c2 / 3.0;
    let q = 2.0 * c2.powi(3) / 27.0 - c2 * c1 / 3.0 + c0;
    let delta = (q / 2.0).powi(2) + (p / 3.0).powi(3);
    let mut root = if delta >= 0.0 {
        let radical = delta.sqrt();
        (-q / 2.0 + radical).cbrt() + (-q / 2.0 - radical).cbrt() - c2 / 3.0
    } else {
        let radius = 2.0 * (-p / 3.0).sqrt();
        let argument = 3.0 * q / (2.0 * p) * (-3.0 / p).sqrt();
        let angle = argument.clamp(-1.0, 1.0).acos() / 3.0;
        radius * angle.cos() - c2 / 3.0
    };
    for _ in 0..2 {
        let derivative = cubic_root_derivative(root, attraction, covolume);
        if derivative.abs() > 1e-10 {
            root -= cubic_residual(root, attraction, covolume) / derivative;
        }
    }
    root
}

#[must_use]
pub fn largest_real_root<S: Scalar>(attraction: S, covolume: S) -> S {
    let (a, b) = (attraction.value(), covolume.value());
    let z = largest_real_root_value(a, b);
    let f_z = cubic_root_derivative(z, a, b);
    let f_a = z - b;
    let f_b = z * z - (6.0 * b + 2.0) * z - a + 2.0 * b + 3.0 * b * b;
    let f_zz = 6.0 * z + 2.0 * (b - 1.0);
    let f_za = 1.0;
    let f_zb = 2.0 * z - 6.0 * b - 2.0;
    let f_ab = -1.0;
    let f_bb = -6.0 * z + 2.0 + 6.0 * b;
    let z_a = -f_a / f_z;
    let z_b = -f_b / f_z;
    let z_aa = -(f_zz * z_a * z_a + 2.0 * f_za * z_a) / f_z;
    let z_ab = -(f_zz * z_a * z_b + f_za * z_b + f_zb * z_a + f_ab) / f_z;
    let z_bb = -(f_zz * z_b * z_b + 2.0 * f_zb * z_b + f_bb) / f_z;
    S::chain2(attraction, covolume, z, z_a, z_b, z_aa, z_ab, z_bb)
}


