// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value, json};

use implexity_physics_lbm::moving::boundary::PortKind;

use crate::problem::{FsiProblem, SCHEMA, TimeKind};

pub const MONITOR: &str = "implexity.fsi.dimensionless";

pub const METRICS: [(&str, &str); 9] = [
    ("fsi_reynolds_number", "1"),
    ("fsi_mach_number", "1"),
    ("fsi_tau_plus", "1"),
    ("fsi_lattice_velocity", "1"),
    ("fsi_density_ratio", "1"),
    ("fsi_cauchy_number", "1"),
    ("fsi_velocity_over_solid_wave_speed", "1"),
    ("fsi_macro_steps_per_period", "1"),
    ("fsi_fluid_substeps", "1"),
];

pub const DEFAULT_REYNOLDS_MAX: f64 = 1000.0;

fn norm(v: [f64; 3]) -> f64 {
    v.iter().map(|x| x * x).sum::<f64>().sqrt()
}

fn reference_velocity(p: &FsiProblem) -> (f64, String) {
    let f = &p.fluid;
    if let Some(r) = f.regime {
        return (r.velocity_m_s, "fluid.regime.velocity_m_s".into());
    }
    let mut best = (0.0, String::from("none (fluid at rest)"));
    let mut consider = |u: f64, src: String| {
        if u > best.0 {
            best = (u, src);
        }
    };
    let mut pressures = Vec::new();
    for port in &f.ports {
        match &port.kind {
            PortKind::Velocity { mean_m_s, amplitude_m_s, profile } => {
                let pmax = profile.iter().fold(1.0_f64, |a, b| a.max(b.abs()));
                let u =
                    (norm(*mean_m_s) + norm(*amplitude_m_s)) * if profile.is_empty() { 1.0 } else { pmax };
                consider(u, format!("velocity port {:?}", port.id));
            }
            PortKind::Pressure { mean_pa, amplitude_pa } => {
                pressures.push((mean_pa.abs() + amplitude_pa.abs(), *mean_pa));
            }
        }
    }
    if !pressures.is_empty() {
        let hi = pressures.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
        let lo = pressures.iter().map(|p| p.1).fold(f64::INFINITY, f64::min).min(0.0);
        let dp = (hi - lo).abs().max(pressures.iter().map(|p| p.0).fold(0.0, f64::max));
        consider((2.0 * dp / f.density_kg_m3).sqrt(), "pressure-driven sqrt(2 dp / rho)".into());
    }
    for s in &f.sponges {
        consider(norm(s.velocity_m_s), "sponge far-field velocity".into());
    }
    consider(norm(f.initial_velocity_m_s), "initial velocity".into());
    let g = norm(f.body_acceleration_m_s2);
    if g > 0.0 {
        let h = (0..3)
            .filter(|a| !f.periodic[*a] && f.shape[*a] > 1)
            .map(|a| f.shape[a] as f64 * f.spacing_m)
            .fold(f64::INFINITY, f64::min);
        let h =
            if h.is_finite() { h } else { f.shape.iter().copied().max().unwrap_or(1) as f64 * f.spacing_m };
        consider(
            g * h * h / (8.0 * f.kinematic_viscosity_m2_s),
            "body-force Poiseuille scale g H^2 / (8 nu)".into(),
        );
    }
    for (i, s) in p.solid.supports.iter().enumerate() {
        if let Some(m) = &s.motion {
            let (freq, _) = m.signal();
            let amp = match m {
                crate::problem::solid::Motion::Harmonic { amplitude_m, .. } => norm(*amplitude_m),
                crate::problem::solid::Motion::HarmonicRotation { amplitude_rad, .. } => {
                    amplitude_rad.abs() * reference_length(p).0
                }
            };
            consider(
                2.0 * std::f64::consts::PI * freq * amp,
                format!("prescribed motion of solid.supports[{i}]"),
            );
        }
    }
    best
}

fn reference_length(p: &FsiProblem) -> (f64, String) {
    if let Some(r) = p.fluid.regime {
        return (r.length_m, "fluid.regime.length_m".into());
    }
    let g = &p.solid.grid;
    let axes: Vec<usize> = if p.solid.plane_strain { vec![0, 1] } else { vec![0, 1, 2] };
    let l = axes.iter().map(|a| g.shape[*a] as f64 * g.element_size_m).fold(f64::INFINITY, f64::min);
    (l, "smallest in-plane extent of the solid reference grid".into())
}

#[must_use]
pub fn screening(p: &FsiProblem) -> Value {
    let f = &p.fluid;
    let t = &p.time;
    let dt_f = t.macro_step_s() / p.coupling.substeps as f64;
    let tau = 0.5 + 3.0 * f.kinematic_viscosity_m2_s * dt_f / (f.spacing_m * f.spacing_m);
    let (u, u_src) = reference_velocity(p);
    let (l, l_src) = reference_length(p);
    let c = f.spacing_m / dt_f;
    let lattice_velocity = u / c;
    let mach = lattice_velocity * 3.0_f64.sqrt();
    let reynolds = u * l / f.kinematic_viscosity_m2_s;
    let m = &p.solid.material;
    let rho_s = p.solid.material_density_kg_m3;
    let density_ratio = rho_s / f.density_kg_m3;
    let young =
        if m.kappa0.is_finite() { 9.0 * m.kappa0 * m.mu0 / (3.0 * m.kappa0 + m.mu0) } else { 3.0 * m.mu0 };
    let cauchy = f.density_kg_m3 * u * u / young;
    let wave = (m.mu0 / m.density).sqrt();
    let reynolds_max = f.regime.map_or(DEFAULT_REYNOLDS_MAX, |r| r.reynolds_max);
    let mut warnings: Vec<String> = Vec::new();
    let mut refusals: Vec<String> = Vec::new();
    if tau <= 0.5 + f.tau_min {
        refusals.push(format!("tau+ = {tau:.6} is not above 1/2 + tau_min = {}: refine the time step (more substeps) or coarsen the lattice", 0.5 + f.tau_min));
    } else if tau < 0.51 {
        warnings
            .push(format!("tau+ = {tau:.5} is close to 1/2: TRT/MRT/cumulant stability margins are small"));
    }
    if lattice_velocity > f.lattice_velocity_limit {
        refusals.push(format!(
            "reference velocity {u:.4e} m/s is {lattice_velocity:.4} in lattice units, above lattice_velocity_limit {}: increase the substeps",
            f.lattice_velocity_limit
        ));
    }
    if mach > f.mach_limit {
        refusals.push(format!("Mach number {mach:.4} above mach_limit {}", f.mach_limit));
    }
    if reynolds > reynolds_max {
        warnings.push(format!(
            "Reynolds number {reynolds:.4e} exceeds the declared exact-gradient regime ({reynolds_max:.4e}): unsteady or chaotic flow is refused by the regime detection (nonperiodic_regime); declare a low-Reynolds analogue or scaled model"
        ));
    }
    if density_ratio < 3.0
        && matches!(p.coupling.mode, implexity_solve::multirate_coupling::CouplingMode::Loose { .. })
    {
        warnings.push(format!(
            "density ratio rho_s/rho_f = {density_ratio:.4}: loose coupling is at risk of the added-mass instability; the Schur-ratio preflight decides"
        ));
    }
    if density_ratio < 20.0 && p.solid.inertia_compensation == 0.0 {
        warnings.push(format!(
            "the partially saturated cells move the fluid inside the body with it: its inertia (rho_f per saturated volume, {:.1} % of the solid's) adds to the solid's; set solid.inertia_compensation to compensate",
            100.0 / density_ratio
        ));
    }
    if matches!(
        f.turbulence,
        implexity_physics_lbm::moving::field::Turbulence::Smagorinsky(_)
            | implexity_physics_lbm::moving::field::Turbulence::Wale(_)
    ) {
        warnings.push("an eddy-viscosity closure is selected: gradients exist for the regularised closure, but turbulent regimes are refused by the regime detection".into());
    }
    let steps_per_period = match t.kind {
        TimeKind::SteadyStability { .. } => 1,
        _ => t.steps_per_period,
    };
    if matches!(t.kind, TimeKind::PeriodicForced | TimeKind::PeriodicAutonomous { .. })
        && steps_per_period < 20
    {
        warnings.push(format!("{steps_per_period} macro steps per period resolve the oscillation coarsely (design guidance: at least 200)"));
    }
    json!({
        "schema": "implexity-fsi-regime-screening/1",
        "reference_velocity_m_s": u, "reference_velocity_source": u_src,
        "reference_length_m": l, "reference_length_source": l_src,
        "fluid_step_s": dt_f, "macro_step_s": t.macro_step_s(),
        "metrics": {
            "fsi_reynolds_number": reynolds, "fsi_mach_number": mach, "fsi_tau_plus": tau,
            "fsi_lattice_velocity": lattice_velocity, "fsi_density_ratio": density_ratio,
            "fsi_cauchy_number": cauchy, "fsi_velocity_over_solid_wave_speed": u / wave,
            "fsi_macro_steps_per_period": steps_per_period, "fsi_fluid_substeps": p.coupling.substeps
        },
        "reynolds_max": reynolds_max,
        "young_modulus_pa": young, "solid_shear_wave_speed_m_s": wave,
        "regime": if reynolds <= reynolds_max { "declared_exact_gradient_regime" } else { "outside_declared_regime" },
        "warnings": warnings,
        "refusals": refusals,
        "admissible": refusals.is_empty(),
    })
}


pub fn monitor(problem: &Value, _diagnostics: Option<&Value>) -> Result<Value, String> {
    if problem.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Ok(Value::Object(Map::new()));
    }
    let p = crate::problem::normalise(problem).map_err(|e| e.message().to_string())?;
    Ok(screening(&p)["metrics"].clone())
}
