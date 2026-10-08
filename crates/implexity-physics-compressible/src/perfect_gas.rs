// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#[must_use]
pub fn primitive_and_stagnation(gamma: f64, gas: f64, temperature: f64, pressure: f64, mach: f64) -> ([f64; 3], f64, f64) {
    let factor = 1.0 + 0.5 * (gamma - 1.0) * mach * mach;
    ([pressure / (gas * temperature), mach * (gamma * gas * temperature).sqrt(), pressure],
     pressure * factor.powf(gamma / (gamma - 1.0)), temperature * factor)
}
