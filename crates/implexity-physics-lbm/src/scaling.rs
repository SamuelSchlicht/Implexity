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


pub fn bernoulli_speed(pressure: f64, density: f64) -> f64 {
    (2.0 * pressure / density).sqrt()
}
#[derive(Clone, Copy, Debug)]
pub struct ReferenceFlowScales {
    pub speed_m_s: f64,
    pub effective_viscosity_m2_s: f64,
    pub reynolds: f64,
    pub physical_mach: f64,
    pub fluid_step_s: f64,
    pub tau_plus: f64,
    pub macro_step_s: f64,
    pub substeps: usize,
    pub updates_per_period: f64,
}
#[allow(clippy::too_many_arguments)]
pub fn reference_flow_scales(
    pressure: f64,
    density: f64,
    viscosity: f64,
    viscosity_factor: f64,
    length: f64,
    spacing: f64,
    lattice_velocity: f64,
    frequency: f64,
    macro_steps: usize,
    cells: usize,
    sound_speed: f64,
) -> ReferenceFlowScales {
    let speed = bernoulli_speed(pressure, density);
    let nu = viscosity_factor * viscosity;
    let reynolds = reynolds_number(speed, length, nu);
    let fluid_step = lattice_velocity * spacing / speed;
    let tau = 0.5 + 3.0 * nu * fluid_step / (spacing * spacing);
    let macro_step = 1.0 / (frequency * macro_steps as f64);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let substeps = (macro_step / fluid_step).ceil().max(1.0) as usize;
    ReferenceFlowScales {
        speed_m_s: speed,
        effective_viscosity_m2_s: nu,
        reynolds,
        physical_mach: mach_number(speed, sound_speed),
        fluid_step_s: fluid_step,
        tau_plus: tau,
        macro_step_s: macro_step,
        substeps,
        updates_per_period: cells as f64 * (substeps * macro_steps) as f64,
    }
}

pub fn reynolds_number(speed: f64, length: f64, viscosity: f64) -> f64 {
    speed * length / viscosity
}
pub fn mach_number(speed: f64, sound_speed: f64) -> f64 {
    speed / sound_speed
}

use implexity_core::{CaeError, CaeResult};
pub const LATTICE_SOUND_SPEED: f64 = 0.577_350_269_189_625_8;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MultirateResolutionRequest {
    pub cells_per_chord: usize,
    pub lattice_speed: f64,
    pub steps_per_period: usize,
    pub tau_min: f64,
    pub mach_limit: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MultirateResolutionPlan {
    pub spacing_m: f64,
    pub period_s: f64,
    pub steps_per_period: usize,
    pub macro_step_s: f64,
    pub substeps: usize,
    pub fluid_step_s: f64,
    pub tau_plus: f64,
    pub lattice_speed: f64,
    pub mach: f64,
    pub fluid_steps_per_convective_time: f64,
}


pub fn multirate_resolution(request: &MultirateResolutionRequest, chord_m: f64, speed_m_s: f64, max_speed_m_s: f64, kinematic_viscosity_m2_s: f64, period_s: f64) -> CaeResult<MultirateResolutionPlan> {
    if request.cells_per_chord < 4 || request.steps_per_period < 8 {
        return Err(CaeError::contract(
            "resolution needs at least 4 cells per chord and 8 macro steps per period",
        ));
    }
    for (v, what) in [
        (request.lattice_speed, "lattice_speed"),
        (chord_m, "chord"),
        (speed_m_s, "free-stream speed"),
        (max_speed_m_s, "largest fluid speed"),
        (kinematic_viscosity_m2_s, "kinematic viscosity"),
        (period_s, "period"),
        (request.mach_limit, "mach_limit"),
    ] {
        if !(v.is_finite() && v > 0.0) {
            return Err(CaeError::contract(format!("resolution {what} must be finite and positive")));
        }
    }
    if !(request.tau_min.is_finite() && request.tau_min >= 0.0) || request.lattice_speed > 0.3 {
        return Err(CaeError::contract("resolution needs tau_min >= 0 and lattice_speed <= 0.3"));
    }
    let spacing = chord_m / request.cells_per_chord as f64;
    let macro_step = period_s / request.steps_per_period as f64;
    let ratio = macro_step * max_speed_m_s / (request.lattice_speed * spacing);
    if !(ratio.is_finite() && ratio < 1e7) {
        return Err(CaeError::contract("resolution needs fewer than 1e7 fluid substeps per macro step"));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let substeps = (ratio.ceil() as usize).max(1);
    let fluid_step = macro_step / substeps as f64;
    let tau_plus = 0.5 + 3.0 * kinematic_viscosity_m2_s * fluid_step / (spacing * spacing);
    let lattice_speed = max_speed_m_s * fluid_step / spacing;
    let mach = lattice_speed / LATTICE_SOUND_SPEED;
    if tau_plus <= 0.5 + request.tau_min {
        return Err(CaeError::contract(format!(
            "relaxation time tau+ = {tau_plus:.6} is not above 1/2 + tau_min = {:.6}: raise the viscosity (lower \
             Reynolds number), the cells per chord or the lattice speed",
            0.5 + request.tau_min
        )));
    }
    if mach > request.mach_limit {
        return Err(CaeError::contract(format!(
            "lattice Mach number {mach:.4} exceeds the limit {}: lower lattice_speed",
            request.mach_limit
        )));
    }
    Ok(MultirateResolutionPlan {
        spacing_m: spacing,
        period_s,
        steps_per_period: request.steps_per_period,
        macro_step_s: macro_step,
        substeps,
        fluid_step_s: fluid_step,
        tau_plus,
        lattice_speed,
        mach,
        fluid_steps_per_convective_time: chord_m / speed_m_s / fluid_step,
    })
}
pub fn dynamic_pressure(density: f64, speed: f64) -> f64 { 0.5 * density * speed * speed }

pub fn lattice_velocity_from_mach_limit(velocity_limit: f64, mach_limit: f64, safety_margin: f64) -> f64 {
    safety_margin * velocity_limit.min(mach_limit / 3.0_f64.sqrt())
}
