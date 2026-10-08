// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex};

use serde_json::{Value, json};

use crate::problem::SCHEMA;

pub type TemplateSource = fn(Option<&Value>) -> Vec<Value>;

static SOURCES: LazyLock<Mutex<BTreeMap<String, TemplateSource>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

pub fn register_template_source(package_id: &str, source: TemplateSource) {
    if let Ok(mut m) = SOURCES.lock() {
        m.insert(package_id.to_string(), source);
    }
}

fn silicone() -> Value {
    json!({"law": "mooney_rivlin", "c10_Pa": 2000.0, "c01_Pa": 500.0,
        "volumetric": {"function": "quadratic", "bulk_Pa": 250_000.0}, "density_kg_m3": 1100.0,
        "label": "synthetic soft silicone-like elastomer (not calibrated)"})
}

fn responses(window: &Value, terms: &Value) -> Value {
    json!({"schema": implexity_solve::dynamic_program::SCHEMA, "window": window, "terms": terms})
}

fn channel_flap(
    time: &Value,
    inflow_amplitude: f64,
    frequency_hz: f64,
    observables: &Value,
    program: &Value,
) -> Value {
    let dx = 5e-4;
    json!({
        "schema": SCHEMA,
        "label": "soft flap in a pulsating channel flow",
        "provenance": "synthetic starter (implexity-physics-fsi templates); not a validated model",
        "fluid": {
            "lattice": "D2Q9", "shape": [60, 24, 1], "spacing_m": dx, "periodic_axes": [false, false, true],
            "density_kg_m3": 1000.0, "kinematic_viscosity_m2_s": 5e-5,
            "collision": {"kind": "trt", "magic": 0.1875}, "coupling_law": {"kind": "psm_superposition"},
            "walls": [{"kind": "box", "box_m": [[0.0, 0.0, -1.0], [0.03, 0.001, 1.0]]}],
            "ports": [
                {"kind": "velocity", "id": "inlet", "face": "xmin", "mean_m_s": [0.02, 0.0, 0.0],
                 "amplitude_m_s": [inflow_amplitude, 0.0, 0.0], "frequency_hz": frequency_hz, "profile": "parabolic"},
                {"kind": "pressure", "id": "outlet", "face": "xmax", "pressure_pa": 0.0}
            ],
            "sponges": [{"face": "xmax", "thickness_cells": 8, "strength": 0.05}],
            "regime": {"velocity_m_s": 0.03, "length_m": 0.001, "reynolds_max": 200.0}
        },
        "solid": {
            "reference_grid": {"origin_m": [0.012, 0.001, 0.0], "shape": [2, 12, 1], "element_size_m": dx},
            "plane_strain": true, "material": silicone(), "formulation": "mixed_up",
            "rayleigh": {"alpha_mass_s_inv": 0.0, "beta_stiffness_s": 1e-4},
            "supports": [{"box_m": [[0.0, 0.0, -1.0], [1.0, 0.00101, 1.0]], "components": [true, true, true]}],

            "integrator": {"kind": "generalized_alpha", "rho_inf": 0.5}
        },
        "design": {"region": "all", "initial_density": 0.8, "filter_radius_m": 7.5e-4,
                   "projection": {"beta": 2.0, "eta": 0.5}, "fluid_blocking": {"beta": 4.0, "eta": 0.5}},
        "coupling": {"mode": "strong_newton_krylov", "substeps": 10},
        "time": time,
        "observables": observables,
        "responses": program,
        "frames": {"count": 8, "fields": ["speed", "occupancy", "solid_displacement"]}
    })
}

fn flap_observables() -> Value {
    json!([
        {"name": "tip_x", "kind": "probe_displacement", "point_m": [0.0125, 0.0069, 0.00025], "component": 0},
        {"name": "drag", "kind": "solid_force", "component": 0},
        {"name": "flux", "kind": "section_flux", "axis": 0, "index": 50}
    ])
}

#[must_use]
pub fn forced_flap() -> Value {
    channel_flap(
        &json!({"kind": "periodic_forced", "period_s": 0.2, "steps_per_period": 40,
               "method": {"kind": "newton_krylov", "krylov_dimension": 30}, "spin_up_periods": 2,
               "tolerance": 1e-8, "adjoint_tolerance": 1e-9}),
        0.01,
        5.0,
        &flap_observables(),
        &responses(
            &json!({"kind": "periodic"}),
            &json!([
                {"name": "tip_amplitude_m", "functional": {"kind": "harmonic", "sample": "tip_x", "order": 1, "part": "amplitude"}},
                {"name": "mean_drag_N", "functional": {"kind": "mean", "sample": "drag"}},
                {"name": "volume_fraction", "functional": {"kind": "design_volume_fraction"}}
            ]),
        ),
    )
}

#[must_use]
pub fn self_oscillation() -> Value {
    channel_flap(
        &json!({"kind": "periodic_autonomous", "period_guess_s": 0.2, "period_bounds_s": [0.02, 2.0],
               "steps_per_period": 40, "phase_condition": {"kind": "section", "sample": "tip_x", "level": 0.0},
               "method": {"kind": "newton_krylov", "krylov_dimension": 40}, "spin_up_periods": 4}),
        0.0,
        0.0,
        &flap_observables(),
        &responses(
            &json!({"kind": "periodic"}),
            &json!([
                {"name": "frequency_hz", "functional": {"kind": "frequency"}},
                {"name": "tip_amplitude_m", "functional": {"kind": "harmonic", "sample": "tip_x", "order": 1, "part": "amplitude"}},
                {"name": "volume_fraction", "functional": {"kind": "design_volume_fraction"}}
            ]),
        ),
    )
}

#[must_use]
pub fn onset_stability() -> Value {
    channel_flap(
        &json!({"kind": "steady_stability", "step_s": 0.005, "modes": 4}),
        0.0,
        0.0,
        &flap_observables(),
        &responses(
            &json!({"kind": "periodic"}),
            &json!([
                {"name": "steady_tip_x_m", "functional": {"kind": "mean", "sample": "tip_x"}},
                {"name": "volume_fraction", "functional": {"kind": "design_volume_fraction"}}
            ]),
        ),
    )
}

#[must_use]
pub fn heaving_plate() -> Value {
    let dx = 5e-4;
    json!({
        "schema": SCHEMA,
        "label": "flexible plate heaved in a free stream",
        "provenance": "synthetic starter (implexity-physics-fsi templates); not a validated model",
        "fluid": {
            "lattice": "D2Q9", "shape": [64, 40, 1], "spacing_m": dx, "periodic_axes": [false, true, true],
            "density_kg_m3": 1000.0, "kinematic_viscosity_m2_s": 5e-5,
            "ports": [
                {"kind": "velocity", "id": "inflow", "face": "xmin", "mean_m_s": [0.02, 0.0, 0.0]},
                {"kind": "pressure", "id": "outflow", "face": "xmax", "pressure_pa": 0.0}
            ],
            "sponges": [{"face": "xmax", "thickness_cells": 8, "strength": 0.05}],
            "regime": {"velocity_m_s": 0.03, "length_m": 0.006, "reynolds_max": 500.0}
        },
        "solid": {
            "reference_grid": {"origin_m": [0.008, 0.0095, 0.0], "shape": [12, 2, 1], "element_size_m": dx},
            "plane_strain": true, "material": silicone(), "formulation": "mixed_up",
            "rayleigh": {"alpha_mass_s_inv": 0.0, "beta_stiffness_s": 1e-4},
            "supports": [{"box_m": [[0.0, 0.0, -1.0], [0.00801, 1.0, 1.0]], "components": [true, true, true],
                          "motion": {"kind": "harmonic", "amplitude_m": [0.0, 0.0005, 0.0], "frequency_hz": 5.0}}],
            "integrator": {"kind": "generalized_alpha", "rho_inf": 0.5}
        },
        "design": {"region": "all", "initial_density": 0.8, "filter_radius_m": 7.5e-4,
                   "fluid_blocking": {"beta": 4.0, "eta": 0.5}},
        "coupling": {"mode": "strong_newton_krylov", "substeps": 10},
        "time": {"kind": "periodic_forced", "period_s": 0.2, "steps_per_period": 40, "spin_up_periods": 2},
        "observables": [
            {"name": "force_x", "kind": "solid_force", "component": 0},
            {"name": "force_y", "kind": "solid_force", "component": 1},
            {"name": "tip_y", "kind": "probe_displacement", "point_m": [0.0139, 0.01, 0.00025], "component": 1}
        ],
        "responses": responses(&json!({"kind": "periodic"}), &json!([
            {"name": "mean_force_x_N", "functional": {"kind": "mean", "sample": "force_x"}},
            {"name": "tip_amplitude_m", "functional": {"kind": "harmonic", "sample": "tip_y", "order": 1, "part": "amplitude"}},
            {"name": "volume_fraction", "functional": {"kind": "design_volume_fraction"}}
        ])),
        "frames": {"count": 8, "fields": ["speed", "occupancy"]}
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TurekHronCase {
    pub id: &'static str,
    pub rho_s: f64,
    pub mu_s: f64,
    pub velocity: f64,
    pub period_s: f64,
}

pub const TUREK_HRON: [TurekHronCase; 3] = [
    TurekHronCase { id: "FSI1", rho_s: 1000.0, mu_s: 0.5e6, velocity: 0.2, period_s: 1.0 },
    TurekHronCase { id: "FSI2", rho_s: 10000.0, mu_s: 0.5e6, velocity: 1.0, period_s: 0.5 },
    TurekHronCase { id: "FSI3", rho_s: 1000.0, mu_s: 2.0e6, velocity: 2.0, period_s: 1.0 / 5.3 },
];

#[must_use]
pub fn turek_hron_fsi2(cells_per_height: usize, substeps: usize, periods: usize) -> Value {
    turek_hron(&TUREK_HRON[1], cells_per_height, substeps, periods, 50)
}

#[must_use]
pub fn turek_hron(
    case: &TurekHronCase,
    cells_per_height: usize,
    substeps: usize,
    periods: usize,
    steps_per_period: usize,
) -> Value {
    let h = 0.41;
    let dx = h / cells_per_height as f64;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let nx = (2.5 / dx).round() as usize;
    let lambda = 2.0 * case.mu_s * 0.4 / (1.0 - 2.0 * 0.4);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (bar_cells, thick) = (((0.35 / dx).round() as usize).max(2), ((0.02 / dx).round() as usize).max(2));
    let x0 = 0.2 + (0.05f64.powi(2) - 0.01f64.powi(2)).sqrt();
    let y0 = 0.2 - 0.5 * thick as f64 * dx;

    let tip_x = (x0 + bar_cells as f64 * dx).min(0.6);
    let rows = periods * steps_per_period;
    let u = case.velocity;


    let compensation = if case.rho_s >= 2.0 * 1000.0 { 1.0 } else { 0.0 };
    json!({
        "schema": SCHEMA,
        "label": format!("Turek-Hron {} at H/dx = {cells_per_height}", case.id),
        "provenance": format!("Turek, Hron (2006) {}: rho_s = {} kg/m3, mu_s = {} Pa, U = {u} m/s, St. Venant-Kirchhoff bar (doi:10.1007/3-540-34596-5_15); reduced resolution", case.id, case.rho_s, case.mu_s),
        "fluid": {
            "lattice": "D2Q9", "shape": [nx, cells_per_height, 1], "spacing_m": dx, "periodic_axes": [false, false, true],
            "density_kg_m3": 1000.0, "kinematic_viscosity_m2_s": 0.001,
            "walls": [{"kind": "cylinder", "center_m": [0.2, 0.2, 0.0], "radius_m": 0.05, "axis": 2}],
            "ports": [
                {"kind": "velocity", "id": "inflow", "face": "xmin", "mean_m_s": [1.5 * u, 0.0, 0.0], "profile": "parabolic", "ramp_s": 2.0},
                {"kind": "pressure", "id": "outflow", "face": "xmax", "pressure_pa": 0.0}
            ],
            "sponges": [{"face": "xmax", "thickness_cells": (cells_per_height / 3).max(4), "strength": 0.05}],
            "lattice_velocity_limit": 0.15, "mach_limit": 0.3,
            "regime": {"velocity_m_s": u, "length_m": 0.1, "reynolds_max": 1.5 * u * 0.1 / 0.001}
        },
        "solid": {
            "reference_grid": {"origin_m": [x0, y0, 0.0], "shape": [bar_cells, thick, 1], "element_size_m": dx},
            "plane_strain": true,
            "material": {"law": "st_venant_kirchhoff", "mu_Pa": case.mu_s, "lambda_Pa": lambda, "density_kg_m3": case.rho_s},
            "formulation": "displacement",
            "supports": [{"box_m": [[0.0, 0.0, -1.0], [x0 + 1e-9, 1.0, 1.0]], "components": [true, true, true]}],
            "integrator": {"kind": "avf_midpoint", "gauss_points": 2},
            "inertia_compensation": compensation
        },
        "design": {"region": "all", "initial_density": 1.0, "fluid_blocking": {"beta": 0.0}},
        "coupling": {"mode": "strong_newton_krylov", "substeps": substeps,
                     "pushforward": {"points_per_axis": 2, "blocking_scale": 1.2}},
        "time": {"kind": "fixed_horizon", "period_s": case.period_s, "steps_per_period": steps_per_period, "periods": periods,
                 "autonomous": true, "checkpoint": {"policy": "binomial", "ram_snapshots": 32}},
        "observables": [
            {"name": "uy_A", "kind": "probe_displacement", "point_m": [tip_x, 0.2, 0.5 * dx], "component": 1},
            {"name": "ux_A", "kind": "probe_displacement", "point_m": [tip_x, 0.2, 0.5 * dx], "component": 0},
            {"name": "lift_bar", "kind": "solid_force", "component": 1},
            {"name": "drag_bar", "kind": "solid_force", "component": 0}
        ],
        "responses": responses(
            &json!({"kind": "hann", "from": rows / 2, "to": rows}),
            &json!([
                {"name": "uy_A_mean_m", "functional": {"kind": "mean", "sample": "uy_A"}},
                {"name": "uy_A_rms_m", "functional": {"kind": "rms", "sample": "uy_A"}},
                {"name": "uy_A_crossing_period_s", "functional": {"kind": "crossing_period", "sample": "uy_A", "level": 0.0}},
                {"name": "volume_fraction", "functional": {"kind": "design_volume_fraction"}}
            ])
        ),
        "frames": {"count": 12, "fields": ["speed", "occupancy", "solid_displacement"]}
    })
}

fn row(id: &str, label: &str, description: &str, patch: &Value) -> Value {
    json!({"id": id, "label": label, "description": description,
        "truth_status": "unvalidated_synthetic_starter",
        "problem_requirements": [{"path": ["schema"], "value": SCHEMA}],
        "problem_patch": patch})
}

#[must_use]
pub fn builtin() -> Vec<Value> {
    vec![
        row(
            "fsi_forced_flap",
            "Soft flap under pulsating flow (forced periodic orbit)",
            "Two-way coupled soft flap in a quasi-2-D channel with a 5 Hz pulsating inflow; exact periodic-orbit gradients of the first-harmonic tip amplitude and the cycle-mean drag (Newton-Krylov shooting).",
            &forced_flap(),
        ),
        row(
            "fsi_self_oscillation",
            "Self-excited flap oscillation (autonomous orbit)",
            "The flap at a steady inflow as an autonomous periodic orbit: the period is an unknown with an exact gradient (frequency response). Requires a self-oscillating regime; refused otherwise (nonperiodic_regime).",
            &self_oscillation(),
        ),
        row(
            "fsi_onset_stability",
            "Onset of oscillation (steady state and least stable mode)",
            "Steady state of the macro-step map and the growth rate of its least stable modes (exact growth-rate gradient through the second-order adjoint of the coupled step).",
            &onset_stability(),
        ),
        row(
            "fsi_heaving_plate",
            "Flexible plate heaved in a free stream",
            "A flexible plate whose leading edge is heaved harmonically in a uniform stream (periodic lateral boundaries): cycle-mean streamwise force (thrust is its negative) and tip amplitude on the forced orbit.",
            &heaving_plate(),
        ),
        row(
            "turek_hron_fsi2_reduced",
            "Turek-Hron FSI2 (reduced resolution)",
            "The FSI2 benchmark geometry (rho_s/rho_f = 10, Re = 100) at H/dx = 41 from rest with the inflow ramp; point-A displacement statistics over a Hann window (validation starter; the tolerances of FSI_DYNAMIC_TOPOLOGY.md C-3 need H/dx = 164).",
            &turek_hron_fsi2(41, 20, 8),
        ),
    ]
}

#[must_use]
pub fn merge_patch(base: &Value, target: &Value) -> Value {
    match (base, target) {
        (Value::Object(b), Value::Object(t)) => {
            let mut out = serde_json::Map::new();
            for (k, tv) in t {
                match b.get(k) {
                    Some(bv) if bv == tv => {}
                    Some(bv) => {
                        out.insert(k.clone(), merge_patch(bv, tv));
                    }
                    None => {
                        out.insert(k.clone(), tv.clone());
                    }
                }
            }
            for k in b.keys() {
                if !t.contains_key(k) {
                    out.insert(k.clone(), Value::Null);
                }
            }
            Value::Object(out)
        }
        _ => target.clone(),
    }
}

#[must_use]
pub fn study_templates(problem: Option<&Value>) -> Vec<Value> {
    let mut rows = builtin();
    let selected = implexity_core::packages::global().selected();
    let sources: Vec<(String, TemplateSource)> =
        SOURCES.lock().map(|m| m.iter().map(|(k, v)| (k.clone(), *v)).collect()).unwrap_or_default();
    for (package, source) in sources {
        if selected.contains(&package) {
            rows.extend(source(problem));
        }
    }
    for row in &mut rows {
        let normal = crate::problem::normalise(&row["problem_patch"]).map(|p| p.normal_form().clone());
        if let Ok(target) = normal {
            row["problem_patch"] = match problem {
                Some(base) if base.is_object() => merge_patch(base, &target),
                _ => target,
            };
        }
    }
    rows
}
