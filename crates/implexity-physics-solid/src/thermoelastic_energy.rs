// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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

