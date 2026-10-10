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



use implexity_ad::{HyperDual, forward};


pub trait HelmholtzPotential {
    fn energy(&self, strain_mandel: &[HyperDual; 6], temperature: HyperDual) -> HyperDual;
}

impl<F> HelmholtzPotential for F
where
    F: Fn(&[HyperDual; 6], HyperDual) -> HyperDual,
{
    fn energy(&self, strain_mandel: &[HyperDual; 6], temperature: HyperDual) -> HyperDual {
        self(strain_mandel, temperature)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThermoelasticResponse {
    pub helmholtz_energy_j_m3: f64,
    pub stress_mandel_pa: [f64; 6],
    pub entropy_j_m3_k: f64,
    pub internal_energy_j_m3: f64,
    pub constant_strain_heat_capacity_j_m3_k: f64,
    pub stress_temperature_tangent_pa_k: [f64; 6],
    pub reversible_heat_source_w_m3: f64,
    pub temperature_margin_k: f64,
    pub heat_capacity_margin_j_m3_k: f64,
}

impl ThermoelasticResponse {
    #[must_use]
    pub fn entries(&self) -> Vec<(&'static str, Vec<f64>)> {
        vec![
            ("helmholtz_energy_J_m3", vec![self.helmholtz_energy_j_m3]),
            ("stress_mandel_Pa", self.stress_mandel_pa.to_vec()),
            ("entropy_J_m3_K", vec![self.entropy_j_m3_k]),
            ("internal_energy_J_m3", vec![self.internal_energy_j_m3]),
            ("constant_strain_heat_capacity_J_m3_K", vec![self.constant_strain_heat_capacity_j_m3_k]),
            ("stress_temperature_tangent_Pa_K", self.stress_temperature_tangent_pa_k.to_vec()),
            ("reversible_heat_source_W_m3", vec![self.reversible_heat_source_w_m3]),
            ("temperature_margin_K", vec![self.temperature_margin_k]),
            ("heat_capacity_margin_J_m3_K", vec![self.heat_capacity_margin_j_m3_k]),
        ]
    }
}

#[must_use]
pub fn response<P: HelmholtzPotential + ?Sized>(
    free_energy: &P,
    strain_mandel: &[f64; 6],
    temperature_k: f64,
    strain_rate_s_inv: &[f64; 6],
) -> ThermoelasticResponse {
    let mut state = strain_mandel.to_vec();
    state.push(temperature_k);
    let (energy, gradient, hessian) = forward::hessian(
        |x: &[HyperDual]| {
            let strain = [x[0], x[1], x[2], x[3], x[4], x[5]];
            free_energy.energy(&strain, x[6])
        },
        &state,
    );
    let entropy = -gradient[6];
    let capacity = -temperature_k * hessian[6 * 7 + 6];
    let mut stress = [0.0; 6];
    let mut tangent = [0.0; 6];
    for i in 0..6 {
        stress[i] = gradient[i];
        tangent[i] = hessian[i * 7 + 6];
    }
    let rate: f64 = tangent.iter().zip(strain_rate_s_inv).map(|(a, b)| a * b).sum();
    ThermoelasticResponse {
        helmholtz_energy_j_m3: energy,
        stress_mandel_pa: stress,
        entropy_j_m3_k: entropy,
        internal_energy_j_m3: energy + temperature_k * entropy,
        constant_strain_heat_capacity_j_m3_k: capacity,
        stress_temperature_tangent_pa_k: tangent,
        reversible_heat_source_w_m3: temperature_k * rate,
        temperature_margin_k: temperature_k,
        heat_capacity_margin_j_m3_k: capacity,
    }
}


pub fn check_response(values: &ThermoelasticResponse) -> Result<(), String> {
    for (name, value) in values.entries() {
        if value.is_empty() || !value.iter().all(|v| v.is_finite()) {
            return Err(format!("Nonfinite or empty thermoelastic response: {name}"));
        }
    }
    for (name, value) in [
        ("temperature_margin_K", values.temperature_margin_k),
        ("heat_capacity_margin_J_m3_K", values.heat_capacity_margin_j_m3_k),
    ] {
        if value <= 0.0 {
            return Err(format!("Thermoelastic state outside positive {name}"));
        }
    }
    Ok(())
}

