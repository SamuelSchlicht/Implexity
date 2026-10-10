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


