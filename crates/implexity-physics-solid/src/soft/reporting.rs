// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


pub fn shear_modulus(young: f64, nu: f64) -> f64 {
    young / (2.0 * (1.0 + nu))
}
pub fn bulk_modulus(young: f64, nu: f64) -> f64 {
    young / (3.0 * (1.0 - 2.0 * nu))
}
pub fn wave_speeds(bulk: f64, shear: f64, density: f64) -> (f64, f64) {
    (((bulk + 4.0 * shear / 3.0) / density).sqrt(), (shear / density).sqrt())
}
pub fn complex_young_modulus(young: f64, nu: f64, branches: &[(f64, f64)], frequency_hz: f64) -> (f64, f64) {
    if branches.is_empty() || frequency_hz == 0.0 {
        return (young, 0.0);
    }
    let omega = 2.0 * std::f64::consts::PI * frequency_hz;
    let mut storage = 1.0;
    let mut loss = 0.0;
    for &(beta, tau) in branches {
        let x = omega * tau;
        let den = 1.0 + x * x;
        storage += beta * x * x / den;
        loss += beta * x / den;
    }
    let g = shear_modulus(young, nu);
    let k = bulk_modulus(young, nu);
    let (a, b) = (g * storage, g * loss);
    let d = 3.0 * k + a;
    let den = d * d + b * b;
    (9.0 * k * (a * d + b * b) / den, 27.0 * k * k * b / den)
}

pub fn young_from_moduli(bulk: f64, shear: f64) -> f64 {
    9.0 * bulk * shear / (3.0 * bulk + shear)
}
pub fn load_to_stiffness(pressure: f64, young: f64) -> f64 {
    pressure / young
}
pub fn density_ratio(solid: f64, fluid: f64) -> f64 {
    solid / fluid
}

pub fn mooney_rivlin_from_shear(shear: f64, c01_share: f64) -> (f64, f64) {
    (0.5 * shear * (1.0 - c01_share), 0.5 * shear * c01_share)
}
pub fn yeoh_from_shear(shear: f64, higher: &[f64]) -> Vec<f64> {
    let mut c = vec![0.5 * shear];
    c.extend_from_slice(higher);
    c
}

pub fn plate_bending_rigidity(young: f64, thickness: f64, nu: f64) -> f64 {
    young * thickness.powi(3) / (12.0 * (1.0 - nu * nu))
}
pub fn areal_mass(density: f64, thickness: f64) -> f64 { density * thickness }
pub fn cantilever_frequency(beta_length: f64, length: f64, rigidity: f64, areal_mass: f64) -> f64 {
    let b = beta_length / length;
    b * b / (2.0 * std::f64::consts::PI) * (rigidity / areal_mass).sqrt()
}
pub fn rigid_plate_added_mass(coefficient: f64, fluid_density: f64, length: f64) -> f64 {
    coefficient * fluid_density * std::f64::consts::PI * length.powi(2) / 4.0
}
pub fn added_mass_frequency(frequency: f64, added: f64, structural_mass: f64) -> f64 {
    frequency / (1.0 + added / structural_mass).sqrt()
}
pub fn cantilever_tip_load(rigidity: f64, deflection: f64, length: f64) -> f64 {
    3.0 * rigidity * deflection / length.powi(3)
}
pub fn cantilever_rigidity(load: f64, length: f64, deflection: f64) -> f64 {
    load * length.powi(3) / (3.0 * deflection)
}
pub fn lame_lambda(young: f64, nu: f64) -> f64 {
    young * nu / ((1.0 + nu) * (1.0 - 2.0 * nu))
}

pub fn cantilever_tip_load_fraction(rigidity: f64, fraction: f64, length: f64) -> f64 {
    3.0 * rigidity * fraction * length / length.powi(3)
}

pub fn constrained_modulus(young: f64, nu: f64) -> f64 {
    young * (1.0 - nu) / ((1.0 + nu) * (1.0 - 2.0 * nu))
}
pub fn plane_strain_modulus(young: f64, nu: f64) -> f64 {
    young / (1.0 - nu * nu)
}
pub fn effective_modulus_from_compliance(stress: f64, volume: f64, compliance: f64) -> f64 {
    stress * stress * volume / compliance
}
