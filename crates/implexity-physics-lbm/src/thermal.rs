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



use implexity_core::{CaeError, CaeResult};

use crate::d3q19::Grid;

#[must_use]
pub fn face_mask(grid: &Grid, periodic: [bool; 3], axis: usize) -> Vec<bool> {
    (0..grid.cells()).map(|x| periodic[axis] || grid.coords(x)[axis] + 1 != grid.shape[axis]).collect()
}

#[must_use]
pub fn roll_axis(grid: &Grid, values: &[f64], axis: usize, shift: i64) -> Vec<f64> {
    let mut s = [0i64; 3];
    s[axis] = shift;
    grid.roll(values, s)
}

#[must_use]
pub fn divergence(grid: &Grid, rates: &[Vec<f64>; 3]) -> Vec<f64> {
    let mut out = vec![0.0; grid.cells()];
    for (axis, rate) in rates.iter().enumerate() {
        let back = roll_axis(grid, rate, axis, 1);
        for x in 0..grid.cells() {
            let term = rate[x] - back[x];
            out[x] = if axis == 0 { term } else { out[x] + term };
        }
    }
    out
}

#[must_use]
pub fn conductances(
    grid: &Grid,
    conductivity: &[f64],
    spacing_m: f64,
    periodic: [bool; 3],
    contact: Option<&[[f64; 3]]>,
) -> [Vec<f64>; 3] {
    std::array::from_fn(|axis| {
        let other = roll_axis(grid, conductivity, axis, -1);
        let mask = face_mask(grid, periodic, axis);
        (0..grid.cells())
            .map(|x| {
                let r = contact.map_or(0.0, |c| c[x][axis]);
                let m = if mask[x] { 1.0 } else { 0.0 };
                m * (spacing_m * spacing_m)
                    / (0.5 * spacing_m / conductivity[x] + r + 0.5 * spacing_m / other[x])
            })
            .collect()
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnergyStep {
    pub temperature_k: Vec<f64>,
    pub capacity_j_k: Vec<f64>,
    pub stored_energy_j: Vec<f64>,
    pub positive_face_power_w: [Vec<f64>; 3],
    pub outgoing_fraction: Vec<f64>,
}

#[must_use]
pub fn energy_step(
    grid: &Grid,
    temperature: &[f64],
    capacity: &[f64],
    conductivity: &[f64],
    mass_rates: &[Vec<f64>; 3],
    cp: f64,
    source_w: &[f64],
    step_s: f64,
    spacing_m: f64,
    periodic: [bool; 3],
) -> EnergyStep {
    let n = grid.cells();
    let g = conductances(grid, conductivity, spacing_m, periodic, None);
    let rates: [Vec<f64>; 3] = std::array::from_fn(|a| {
        let mask = face_mask(grid, periodic, a);
        (0..n).map(|x| mass_rates[a][x] * if mask[x] { 1.0 } else { 0.0 }).collect()
    });
    let mut heat: [Vec<f64>; 3] = std::array::from_fn(|_| vec![0.0; n]);
    let mut outgoing = vec![0.0; n];
    for axis in 0..3 {
        let other = roll_axis(grid, temperature, axis, -1);
        let g_back = roll_axis(grid, &g[axis], axis, 1);
        let m_back = roll_axis(grid, &rates[axis], axis, 1);
        for x in 0..n {
            let m = rates[axis][x];
            heat[axis][x] = g[axis][x] * (temperature[x] - other[x])
                + cp * if m >= 0.0 { m * temperature[x] } else { m * other[x] };
            outgoing[x] = outgoing[x] + g[axis][x] + g_back[x] + cp * (m.max(0.0) + (-m_back[x]).max(0.0));
        }
    }
    let rate_div = divergence(grid, &rates);
    let heat_div = divergence(grid, &heat);
    let next_capacity: Vec<f64> = (0..n).map(|x| capacity[x] - step_s * cp * rate_div[x]).collect();
    let energy: Vec<f64> =
        (0..n).map(|x| capacity[x] * temperature[x] + step_s * (source_w[x] - heat_div[x])).collect();
    EnergyStep {
        temperature_k: (0..n).map(|x| energy[x] / next_capacity[x]).collect(),
        capacity_j_k: next_capacity,
        stored_energy_j: energy,
        positive_face_power_w: heat,
        outgoing_fraction: (0..n).map(|x| step_s * outgoing[x] / capacity[x]).collect(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CheckedStep {
    pub step: EnergyStep,
    pub relative_energy_balance_error: f64,
}


pub fn checked_step(
    grid: &Grid,
    temperature: &[f64],
    capacity: &[f64],
    conductivity: &[f64],
    mass_rates: &[Vec<f64>; 3],
    cp: f64,
    source_w: &[f64],
    step_s: f64,
    spacing_m: f64,
    periodic: [bool; 3],
) -> CaeResult<CheckedStep> {
    if grid.shape.iter().any(|n| *n < 2) {
        return Err(CaeError::contract("Thermal grid requires three dimensions of at least two cells"));
    }
    let n = grid.cells();
    for (name, a) in [
        ("temperature", temperature),
        ("capacity", capacity),
        ("conductivity", conductivity),
        ("source", source_w),
    ] {
        if a.len() != n || !a.iter().all(|v| v.is_finite()) {
            return Err(CaeError::contract(format!("Finite shape-matched thermal {name} required")));
        }
    }
    if [temperature, capacity, conductivity].iter().any(|a| a.iter().any(|v| *v <= 0.0)) {
        return Err(CaeError::contract("Positive absolute temperature, capacity and conductivity required"));
    }
    if [cp, step_s, spacing_m].iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return Err(CaeError::contract("Positive finite specific heat, step and spacing required"));
    }
    if mass_rates.iter().any(|m| m.len() != n || !m.iter().all(|v| v.is_finite())) {
        return Err(CaeError::contract("Three finite shape-matched oriented mass-rate arrays required"));
    }
    for axis in 0..3 {
        let mask = face_mask(grid, periodic, axis);
        if (0..n).any(|x| !mask[x] && mass_rates[axis][x] != 0.0) {
            return Err(CaeError::contract("Nonzero mass flow through a closed exterior face"));
        }
    }
    let out = energy_step(
        grid,
        temperature,
        capacity,
        conductivity,
        mass_rates,
        cp,
        source_w,
        step_s,
        spacing_m,
        periodic,
    );
    let all = [&out.temperature_k, &out.capacity_j_k, &out.stored_energy_j, &out.outgoing_fraction];
    if all.iter().any(|a| !a.iter().all(|v| v.is_finite()))
        || out.positive_face_power_w.iter().any(|a| !a.iter().all(|v| v.is_finite()))
    {
        return Err(CaeError::contract("Nonfinite thermal state"));
    }
    if out.capacity_j_k.iter().any(|v| *v <= 0.0) || out.temperature_k.iter().any(|v| *v <= 0.0) {
        return Err(CaeError::contract("Thermal step produced nonpositive capacity or temperature"));
    }
    if out.outgoing_fraction.iter().fold(f64::NEG_INFINITY, |m, v| m.max(*v)) > 1.0 {
        return Err(CaeError::contract("Thermal monotonicity limit exceeded; reduce the authored step"));
    }
    let before: f64 = (0..n).map(|x| temperature[x] * capacity[x]).sum();
    let after: f64 = out.stored_energy_j.iter().sum();
    let expected = step_s * source_w.iter().sum::<f64>();
    let drift = (after - before - expected).abs() / before.abs().max(expected.abs()).max(1e-30);
    if drift > 1e-6 {
        return Err(CaeError::contract("Thermal global energy balance failed"));
    }
    Ok(CheckedStep { step: out, relative_energy_balance_error: drift })
}

