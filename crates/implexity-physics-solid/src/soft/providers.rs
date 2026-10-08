// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use implexity_ad::scan::{Checkpointing, scan_adjoint};
use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, ProviderCapabilities, ProviderDescriptor, ProviderProblem,
    Sensitivity,
};
use implexity_core::coupling_graph::CouplingDeclaration;
use implexity_core::orchestration::{
    AddInCategory, AddInContract, DesignCoordinateRef, ExecutionKind, Fidelity, PublishedContract,
    ResponseCapability, RuntimeRoute,
};
use implexity_core::{CaeError, CaeResult};
use implexity_optim::design::{NamedArrays, design_identity};
use implexity_optim::optimizer::OptimizerLifecycleConfig;
use implexity_optim::provider_ops::{
    AdmissionReply, CandidateDesign, DesignOp, DesignOperations, DesignSensitivities, DesignSensitivity,
    LifecycleDeclaration,
};

use super::design::Interpolation;
use super::problem::{Case, DesignSettings, case, design};
use super::stepper::{Measure, Objective, SoftHistory, StepSolution};
use crate::util::{contract, obj};

pub const DENSITY: &str = "model:soft_density";
pub const ANGLE: &str = "model:fibre_angle_rad";
pub const INSTALLER: &str = "implexity.addins.soft_matter_mechanics";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Static,
    History,
    Design,
}

pub const ALL: [Kind; 3] = [Kind::Static, Kind::History, Kind::Design];

const STATIC_NOTES: [&str; 4] = [
    "Finite-deformation T4 equilibrium: neo-Hookean, Mooney-Rivlin, Yeoh, Ogden, Gent, Arruda-Boyce laws, optional Holzapfel-Gasser-Ogden fibres with dispersion.",
    "Decoupled volumetric functions; the mixed_up formulation (continuous linear pressure, polynomial-pressure-projection stabilisation) avoids volumetric locking for nearly or exactly incompressible solids.",
    "Dead loads and follower pressure on authored faces, load-stepped Newton with residual line search. No contact, no self-contact, no instability (buckling/snap-through) path following.",
    "Calibrated material data are required; convergence does not certify stability, mesh convergence or material validity.",
];
const HISTORY_NOTES: [&str; 4] = [
    "Total-Lagrangian finite-deformation histories: quasistatic, Newmark or Chung-Hulbert generalized-alpha with Rayleigh damping (reference stiffness).",
    "Finite-strain viscoelasticity by Prony series on the isochoric stress (Simo 1987 / Holzapfel 1996 convolution algorithm).",
    "Amplitude-scaled dead loads, follower pressure and prescribed displacements; the energy balance residual reports viscous plus algorithmic dissipation.",
    "No contact, fluid coupling or thermal effects; calibrated damping and relaxation data are required.",
];
const DESIGN_NOTES: [&str; 4] = [
    "Density topology design of finite-deformation solids with Wang's energy interpolation (linear response of low-density elements), SIMP or RAMP stiffness and Pedersen mass interpolation.",
    "Exact gradients of final and weighted time-mean responses through the complete discrete history (implicit-function adjoint per step, checkpointed scan).",
    "Optional fibre-angle design of fibre-reinforced materials on authored rotation axes.",
    "Gradients refer to the admitted discrete equilibrium branch; bifurcations, contact and mesh-dependence are not qualified.",
];

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

impl Kind {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Static => "soft_hyperelastic_static",
            Self::History => "soft_viscoelastic_dynamics",
            Self::Design => "soft_topology_design",
        }
    }

    fn notes(self) -> Vec<String> {
        strings(match self {
            Self::Static => &STATIC_NOTES,
            Self::History => &HISTORY_NOTES,
            Self::Design => &DESIGN_NOTES,
        })
    }

    #[must_use]
    pub fn units(self) -> Vec<(&'static str, &'static str)> {
        match self {
            Self::Static => vec![
                ("stored_energy_J", "J"),
                ("compliance_J", "J"),
                ("peak_displacement_m", "m"),
                ("minimum_J", "1"),
                ("maximum_J", "1"),
                ("free_residual_N", "N"),
            ],
            Self::History => vec![
                ("final_strain_energy_J", "J"),
                ("final_kinetic_energy_J", "J"),
                ("time_mean_strain_energy_J", "J"),
                ("time_mean_kinetic_energy_J", "J"),
                ("external_work_J", "J"),
                ("damping_dissipation_J", "J"),
                ("energy_balance_residual_J", "J"),
                ("viscous_work_J", "J"),
                ("algorithmic_dissipation_J", "J"),
                ("peak_displacement_m", "m"),
                ("minimum_J", "1"),
            ],
            Self::Design => DESIGN_RESPONSES.iter().map(|(k, u, _)| (*k, *u)).collect(),
        }
    }
}

const DESIGN_RESPONSES: [(&str, &str, (&str, bool)); 11] = [
    ("final_compliance_J", "J", ("compliance", false)),
    ("time_mean_compliance_J", "J", ("compliance", true)),
    ("final_strain_energy_J", "J", ("strain_energy", false)),
    ("time_mean_strain_energy_J", "J", ("strain_energy", true)),
    ("final_kinetic_energy_J", "J", ("kinetic_energy", false)),
    ("time_mean_kinetic_energy_J", "J", ("kinetic_energy", true)),
    ("final_displacement_squared_m2", "m2", ("displacement_squared", false)),
    ("time_mean_displacement_squared_m2", "m2", ("displacement_squared", true)),
    ("final_tracking_error_m2", "m2", ("tracking", false)),
    ("time_mean_tracking_error_m2", "m2", ("tracking", true)),
    ("volume_fraction", "1", ("volume", false)),
];

fn arr(values: Vec<f64>, shape: &[usize]) -> Result<ArrayD<f64>, CaeError> {
    ArrayD::from_shape_vec(IxDyn(shape), values)
        .map_err(|e| CaeError::contract(format!("internal array shape error: {e}")))
}

fn field(values: Vec<f64>, shape: &[usize]) -> Result<FieldValue, CaeError> {
    Ok(FieldValue::Array(arr(values, shape)?))
}


pub fn box_mesh(cells: [usize; 3], size: [f64; 3]) -> Result<(Vec<[f64; 3]>, Vec<[usize; 4]>), CaeError> {
    let m = crate::solid_history::mesh(cells)?;
    #[allow(clippy::cast_precision_loss)]
    let points =
        m.ijk.iter().map(|p| core::array::from_fn(|a| p[a] as f64 * size[a] / cells[a] as f64)).collect();
    Ok((points, m.tets))
}

fn beam() -> Result<(Vec<[f64; 3]>, Vec<[usize; 4]>), CaeError> {
    box_mesh([8, 2, 2], [0.04, 0.01, 0.01])
}

#[allow(clippy::float_cmp)]
fn beam_supports(points: &[[f64; 3]]) -> (Vec<[bool; 3]>, Vec<[f64; 3]>) {
    let fixed = points.iter().map(|p| [p[0] == 0.0; 3]).collect();
    let tip: Vec<usize> = (0..points.len()).filter(|k| points[*k][0] == 0.04).collect();
    #[allow(clippy::cast_precision_loss)]
    let share = -0.1 / tip.len() as f64;
    let force =
        (0..points.len()).map(|k| if tip.contains(&k) { [0.0, 0.0, share] } else { [0.0; 3] }).collect();
    (fixed, force)
}

fn silicone(prony: bool) -> Value {
    let mut m = json!({"law": "mooney_rivlin", "c10_Pa": 100_000.0, "c01_Pa": 10_000.0,
        "volumetric": {"function": "quadratic", "bulk_Pa": 50_000_000.0}, "density_kg_m3": 1100.0,
        "label": "Synthetic silicone-like elastomer (not calibrated)"});
    if prony {
        m["prony"] = json!({"beta": [0.3], "tau_s": [0.01]});
    }
    m
}


pub fn static_template() -> Result<Value, CaeError> {
    let (points, elements) = beam()?;
    let (fixed, force) = beam_supports(&points);
    Ok(json!({"points": points, "elements": elements, "materials": [silicone(false)],
        "formulation": "mixed_up", "fixed_dofs": fixed, "nodal_force_N": force, "load_steps": 4,
        "provenance": "Synthetic soft cantilever; not calibrated material data"}))
}


pub fn history_template() -> Result<Value, CaeError> {
    let (points, elements) = beam()?;
    let (fixed, force) = beam_supports(&points);
    let steps = 40;
    #[allow(clippy::cast_precision_loss)]
    let times: Vec<f64> = (0..=steps).map(|k| 0.0025 * f64::from(k)).collect();
    let amplitude: Vec<f64> =
        times[1..].iter().map(|t| (2.0 * std::f64::consts::PI * 10.0 * t).sin()).collect();
    Ok(json!({"points": points, "elements": elements, "materials": [silicone(true)],
        "formulation": "mixed_up", "fixed_dofs": fixed, "nodal_force_N": force,
        "times_s": times, "force_amplitude": amplitude,
        "scheme": {"kind": "generalized_alpha", "rho_inf": 0.8},
        "rayleigh": {"alpha_mass_s_inv": 0.0, "beta_stiffness_s": 0.0001},
        "provenance": "Synthetic soft cantilever under a 10 Hz tip load; not calibrated material data"}))
}


pub fn design_template() -> Result<Value, CaeError> {
    let mut c = static_template()?;
    c["formulation"] = json!("displacement");
    let ne = c["elements"].as_array().map_or(0, Vec::len);
    Ok(json!({"analysis": "static", "case": c,
        "interpolation": {"stiffness": "simp", "penalty": 3.0, "e_min": 1e-6, "wang": true, "wang_beta": 500.0, "wang_eta": 0.01, "mass": "pedersen"},
        "filter_radius_m": 0.0075, "design_region": vec![true; ne], "fixed_density": vec![1.0; ne]}))
}

fn periodic_section(points: &[[f64; 3]]) -> Value {
    let tip =
        points.iter().copied().fold([f64::NEG_INFINITY, 0.0, 0.0], |a, p| if p[0] > a[0] { p } else { a });
    json!({"observables": [
            {"name": "tip_y", "kind": "probe_displacement", "point_m": tip, "component": 1},
            {"name": "strain", "kind": "strain_energy"}],
        "responses": {"schema": "implexity-dynamic-response-program/1", "window": {"kind": "periodic"},
            "terms": [
                {"name": "cycle_mean_strain_energy_J", "functional": {"kind": "mean", "sample": "strain"}},
                {"name": "tip_amplitude_m", "functional": {"kind": "harmonic", "sample": "tip_y", "order": 1, "part": "amplitude"}},
                {"name": "volume", "functional": {"kind": "design_volume_fraction"}}]},
        "method": {"kind": "newton_krylov", "krylov_dimension": 30},
        "tolerance": 1e-10, "adjoint_tolerance": 1e-10, "spin_up_periods": 1, "max_periods": 400,
        "stability_margin": 1e-3, "floquet_modes": 2, "checkpoint": {"policy": "all"}})
}


pub fn periodic_design_template() -> Result<Value, CaeError> {
    let (points, elements) = box_mesh([6, 2, 2], [0.03, 0.01, 0.01])?;
    let fixed: Vec<[bool; 3]> = points.iter().map(|p| [p[0] == 0.0; 3]).collect();
    let tip: Vec<usize> = (0..points.len()).filter(|k| points[*k][0] >= 0.03).collect();
    #[allow(clippy::cast_precision_loss)]
    let share = -0.02 / tip.len() as f64;
    let force: Vec<[f64; 3]> =
        (0..points.len()).map(|k| if tip.contains(&k) { [0.0, share, 0.0] } else { [0.0; 3] }).collect();
    let steps = 24_u32;
    let period = 0.05;
    let times: Vec<f64> = (0..=steps).map(|k| period * f64::from(k) / f64::from(steps)).collect();
    let amplitude: Vec<f64> =
        (1..=steps).map(|k| (2.0 * std::f64::consts::PI * f64::from(k) / f64::from(steps)).sin()).collect();
    let ne = elements.len();
    let case = json!({"points": points, "elements": elements, "materials": [silicone(false)],
        "formulation": "mixed_up", "fixed_dofs": fixed, "nodal_force_N": force,
        "times_s": times, "force_amplitude": amplitude,
        "scheme": {"kind": "generalized_alpha", "rho_inf": 0.8},
        "rayleigh": {"alpha_mass_s_inv": 20.0, "beta_stiffness_s": 0.0001},
        "provenance": "Synthetic soft cantilever under a 20 Hz tip load; not calibrated material data"});
    Ok(json!({"analysis": "history", "case": case,
        "interpolation": {"stiffness": "simp", "penalty": 3.0, "e_min": 1e-6, "wang": true, "wang_beta": 500.0, "wang_eta": 0.01, "mass": "pedersen"},
        "filter_radius_m": 0.0075, "design_region": vec![true; ne], "fixed_density": vec![1.0; ne],
        "periodic": periodic_section(&points)}))
}

#[must_use]
pub fn study_templates(problem: Option<&Value>) -> Vec<Value> {
    let Some(p) = problem.filter(|p| p.get("analysis").and_then(Value::as_str) == Some("history")) else {
        return Vec::new();
    };
    let Some(points) = p["case"]["points"].as_array() else { return Vec::new() };
    let pts: Option<Vec<[f64; 3]>> = points
        .iter()
        .map(|q| {
            let a = q.as_array()?;
            Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2)?.as_f64()?])
        })
        .collect();
    let Some(pts) = pts else { return Vec::new() };
    let patch = json!({"periodic": periodic_section(&pts)});
    let mut merged = p.clone();
    merged["periodic"] = patch["periodic"].clone();
    if design(&merged).is_err() {
        return Vec::new();
    }
    vec![json!({"id": "solid_forced_periodic",
        "label": "Forced periodic responses (cycle mean and first harmonic)",
        "description": "Treats the history loading as one forcing period and optimises responses of the forced periodic orbit (Newton-Krylov shooting, exact periodic adjoint): cycle-mean strain energy, first-harmonic tip amplitude and the volume fraction. Requires uniform steps and enough damping for a stable orbit.",
        "truth_status": "unvalidated_synthetic_starter",
        "problem_requirements": [{"path": ["analysis"], "value": "history"}],
        "problem_patch": patch})]
}

fn describe(rows: &[(&str, &str, &str)]) -> Map<String, Value> {
    rows.iter()
        .map(|(k, title, description)| {
            let format = if *k == "provenance" { "text" } else { "json" };
            ((*k).to_string(), json!({"title": title, "format": format, "description": description}))
        })
        .collect()
}

const COMMON_PROPERTIES: [(&str, &str, &str); 14] = [
    ("points", "Reference mesh nodes (m)", "Node-by-XYZ coordinates in metres."),
    ("elements", "Tetrahedral connectivity", "Zero-based node indices of positively oriented tetrahedra."),
    (
        "materials",
        "Materials",
        "List of {law, parameters, volumetric {function, bulk_Pa[, beta]}, optional fibres {k1_Pa, k2, kappa}, optional prony {beta, tau_s}, density_kg_m3}. Laws: neo_hookean (mu_Pa), mooney_rivlin (c10_Pa, c01_Pa), yeoh (c_Pa list), ogden (mu_Pa, alpha lists), gent (mu_Pa, jm), arruda_boyce (mu_Pa, lambda_m), neo_hookean_coupled (mu_Pa, lambda_Pa; volumetric coupled). Volumetric functions: quadratic, logarithmic, simo_taylor, miehe, ogden_beta, incompressible (mixed_up only).",
    ),
    ("element_material", "Material per element", "One material index per tetrahedron (default 0)."),
    (
        "formulation",
        "Formulation",
        "displacement (T4) or mixed_up (continuous linear pressure with polynomial-pressure-projection stabilisation; use for nearly incompressible hydrogels and silicones).",
    ),
    ("stabilization", "Pressure stabilisation factor", "mixed_up only; default 1."),
    (
        "fibre_directions",
        "Fibre directions",
        "{\"uniform\": [[x,y,z], ...]} or element-by-family-by-XYZ unit vectors (1..4 families) for fibre-reinforced materials.",
    ),
    (
        "fibre_axis",
        "Fibre rotation axis",
        "One XYZ vector or one per element; the fibre-angle design rotates the fibres about it.",
    ),
    ("fixed_dofs", "Prescribed displacement components", "Node-by-XYZ Booleans."),
    (
        "prescribed_displacement_m",
        "Prescribed displacement (m)",
        "Node-by-XYZ values of the fixed components (scaled by the displacement amplitude).",
    ),
    (
        "nodal_force_N",
        "Nodal dead loads (N)",
        "Node-by-XYZ reference-direction forces (scaled by the force amplitude).",
    ),
    (
        "pressure_faces",
        "Follower-pressure faces",
        "[face,3] node indices; the outward normal follows the right-hand rule. Positive pressure pushes against the normal.",
    ),
    (
        "pressure_Pa",
        "Follower pressure (Pa)",
        "Scaled by the pressure amplitude; acts on the current face area and orientation.",
    ),
    (
        "provenance",
        "Material and loading provenance",
        "Calibration source, loading and boundary assumptions.",
    ),
];

fn static_properties() -> Map<String, Value> {
    let mut p = describe(&COMMON_PROPERTIES);
    p.extend(describe(&[
        ("load_steps", "Load steps", "Linear load ramp in 1..1000 increments (one Newton solve each)."),
        (
            "newton",
            "Newton options",
            "{max_iterations (default 50), relative_tolerance (default 1e-10), factorization_reuse (false; true or {contraction (0.5), max_reuse (8)}: modified Newton inside a step, same convergence certificate)}.",
        ),
        ("tracking", "Displacement targets", "{targets: [[node, component, target_m, weight], ...]} for the tracking responses of the design provider."),
    ]));
    p
}

fn history_properties() -> Map<String, Value> {
    let mut p = describe(&COMMON_PROPERTIES);
    p.extend(describe(&[
        ("times_s", "Time points (s)", "0 = t0 < t1 < ... < tN."),
        ("force_amplitude", "Force amplitude", "One multiplier per step (t1..tN); zero at t0."),
        ("displacement_amplitude", "Displacement amplitude", "One multiplier per step for the prescribed displacement."),
        (
            "prescribed_patterns",
            "Additional prescribed displacements",
            "[{displacement_m: [node,3], amplitude: one multiplier per step}]: further motion patterns of the fixed components with their own time signals (added to prescribed_displacement_m × displacement_amplitude).",
        ),
        ("pressure_amplitude", "Pressure amplitude", "One multiplier per step for the follower pressure."),
        ("initial_velocity_m_s", "Initial velocity (m/s)", "Node-by-XYZ; zero on fixed components."),
        ("scheme", "Time integration", "{kind: quasistatic | newmark (beta, gamma) | generalized_alpha (rho_inf) | avf_midpoint (gauss_points 1..8, energy consistent)}."),
        ("rayleigh", "Rayleigh damping", "{alpha_mass_s_inv, beta_stiffness_s}; the stiffness part uses the reference (small-strain) stiffness."),
        ("mass", "Mass matrix", "consistent (default) or lumped."),
        ("newton", "Newton options", "{max_iterations, relative_tolerance, factorization_reuse}."),
        ("output_every", "Output stride", "Store every k-th state in the history fields."),
        ("tracking", "Displacement targets", "{targets: [[node, component, target_m, weight], ...], amplitude: per step}."),
    ]));
    p
}

fn history_series(rows: &[(&str, &str, &str)]) -> Value {
    json!({"kind": "scalar_time_histories", "time_field": "time_s",
        "series": rows.iter().map(|(k, l, u)| json!({"key": k, "label": l, "unit": u})).collect::<Vec<_>>()})
}

fn metadata(kind: Kind) -> Map<String, Value> {
    let d = kind == Kind::Design;
    kind.units()
        .into_iter()
        .map(|(k, u)| (k.to_string(), json!({"unit": u, "differentiable": d, "design_reachable": d})))
        .collect()
}

fn capabilities(kind: Kind) -> Result<ProviderCapabilities, CaeError> {
    let mut d = ProviderDescriptor::new(
        kind.name(),
        vec![kind.name().to_string()],
        kind.units().iter().map(|(k, _)| (*k).to_string()).collect(),
    );
    d.nonlinear = true;
    d.notes = kind.notes();
    d.response_metadata = metadata(kind);
    let (title, template, properties, fields, traits) = match kind {
        Kind::Static => (
            "Soft solid, finite-deformation equilibrium",
            static_template()?,
            static_properties(),
            vec![
                "displacement_m",
                "cauchy_stress_Pa",
                "jacobian",
                "support_reactions_N",
                "hydrostatic_pressure_Pa",
                "fibre_stretch",
            ],
            json!({"experimental": true, "evaluation_only": true}),
        ),
        Kind::History => (
            "Soft solid, dynamics and finite-strain viscoelasticity",
            history_template()?,
            history_properties(),
            vec![
                "time_s",
                "displacement_history_m",
                "velocity_history_m_s",
                "pressure_history_Pa",
                "strain_energy_history_J",
                "kinetic_energy_history_J",
                "external_work_history_J",
                "peak_displacement_history_m",
                "cauchy_stress_Pa",
                "jacobian",
            ],
            json!({"experimental": true, "evaluation_only": true, "history_preview": history_series(&[
                ("strain_energy_history_J", "Strain energy", "J"),
                ("kinetic_energy_history_J", "Kinetic energy", "J"),
                ("peak_displacement_history_m", "Peak displacement", "m"),
            ])}),
        ),
        Kind::Design => {
            d.sensitivities = true;
            d.design_coordinates = vec![DENSITY.to_string(), ANGLE.to_string()];
            let t = design_template()?;
            let ne = t["case"]["elements"].as_array().map_or(0, Vec::len);
            let mut props = describe(&[
                (
                    "analysis",
                    "Analysis",
                    "static (load-stepped equilibrium) or history (dynamics/viscoelasticity).",
                ),
                (
                    "case",
                    "Analysed case",
                    "A complete soft_hyperelastic_static or soft_viscoelastic_dynamics problem.",
                ),
                (
                    "interpolation",
                    "Design interpolation",
                    "{stiffness: simp (penalty) | ramp (q), e_min, wang (true), wang_beta (500), wang_eta (0.01), mass: pedersen | linear | constant}.",
                ),
                (
                    "filter_radius_m",
                    "Density filter radius (m)",
                    "Volume-weighted hat filter; 0 disables filtering.",
                ),
                ("projection", "Heaviside projection", "{beta, eta}; omitted: no projection."),
                ("design_region", "Designable elements", "One Boolean per element."),
                ("fixed_density", "Protected densities", "Used outside the design region."),
                (
                    "fibre_angle",
                    "Fibre-angle design",
                    "{design_region, fixed_rad, filter_radius_m}; enables the coordinate model:fibre_angle_rad.",
                ),
                (
                    "response_weights",
                    "Time-mean weights",
                    "One nonnegative weight per step (default: step lengths); zero weights exclude transients from cycle averages.",
                ),
                ("checkpointing", "Adjoint checkpointing", "all (default), sqrt or an interval."),
                (
                    "periodic",
                    "Forced periodic responses",
                    "History analyses: the loading is one forcing period of uniform steps, or with autonomous: {phase: {kind: section (sample, level) | integral}, period_guess_s, period_bounds_s} a self-excited orbit under time-invariant loads with the period as an unknown; {observables: [{name, kind: probe_displacement (point_m, component) | probe_separation (point_a_m, point_b_m, component) | plane_gap (normal, offset_m, beta) | strain_energy | kinetic_energy | stress_aggregate (p)}], responses: implexity-dynamic-response-program/1 over the observables (design term design_volume_fraction), method: {kind: newton_krylov (krylov_dimension) | newton_picard (subspace) | picard}, tolerance, adjoint_tolerance, spin_up_periods, max_periods, stability_margin, floquet_modes, checkpoint: {policy: all | binomial, ram_snapshots}}. The responses are the program's term names plus volume_fraction; gradients are exact on the discrete periodic orbit.",
                ),
            ]);
            props.insert(
                "analysis".into(),
                json!({"title": "Analysis", "type": "string", "enum": ["static", "history"]}),
            );
            let editor = json!({"kind": "native_json", "title": "Soft-solid topology optimization",
                "design_template": {DENSITY: {"value": vec![0.5; ne], "lower": 0.0, "upper": 1.0, "designable": vec![true; ne]}},
                "problem_template": t, "schema": {"type": "object", "properties": props}});
            d.fields = strings(&["displacement_m", "density_physical", "fibre_angle_rad"]);
            d.traits = obj(json!({"experimental": true, "requires_explicit_design": true}));
            return Ok(ProviderCapabilities::Descriptor(Box::new(d.with_presentation(editor, "array")?)));
        }
    };
    d.fields = strings(&fields);
    d.traits = obj(traits);
    let editor = json!({"kind": "native_json", "title": title, "problem_template": template,
        "schema": {"type": "object", "properties": properties}});

    Ok(ProviderCapabilities::Descriptor(Box::new(d.with_presentation(editor, "array")?)))
}

fn orchestration_contract(kind: Kind) -> Result<AddInContract, CaeError> {
    let name = kind.name();
    let mut c = AddInContract::new(name);
    c.category = AddInCategory::Field;
    let design = kind == Kind::Design;
    let port = format!("{name}.design.0");
    let angle_port = format!("{name}.design.1");
    c.responses = kind
        .units()
        .into_iter()
        .map(|(r, u)| {
            let mut cap = ResponseCapability::new(r);
            cap.unit = u.into();
            cap.differentiable = Some(design);
            cap.design_reachable = Some(design);
            cap.depends_on = if design {
                if r == "volume_fraction" { vec![port.clone()] }
                else { vec![port.clone(), angle_port.clone()] }
            } else { Vec::new() };
            cap
        })
        .collect();
    c.scope = strings(&["mechanics", "soft_matter"]);
    c.fidelity = Fidelity::Screening;
    c.runtime_route = RuntimeRoute::Array;
    c.exact_design_derivatives = Some(design);
    c.exact_state_transpose = Some(design);
    c.notes = kind.notes();
    c.contract_version = 2;
    c.compatibility_mode = false;
    c.owner_id = format!("provider:{name}");
    c.execution_kind = Some(ExecutionKind::Provider);
    c.supported_operations = if design {
        strings(&["preflight", "preflight_design", "evaluate", "sensitivity", "sensitivities", "optimize"])
    } else {
        strings(&["preflight", "evaluate"])
    };
    if design {
        let mut dc = DesignCoordinateRef::new(DENSITY, port.clone());
        dc.addin_id = name.into();
        let mut da = DesignCoordinateRef::new(ANGLE, angle_port);
        da.addin_id = name.into();
        c.design_inputs = vec![dc, da];
    }
    c.checked()
}

fn fmax(v: impl Iterator<Item = f64>) -> f64 {
    v.fold(f64::NEG_INFINITY, f64::max)
}

fn fmin(v: impl Iterator<Item = f64>) -> f64 {
    v.fold(f64::INFINITY, f64::min)
}

fn peak(u: &[f64]) -> f64 {
    fmax(u.chunks(3).map(|c| (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt())).max(0.0)
}

struct FinalFields {
    stress: Vec<f64>,
    jac: Vec<f64>,
    stretch: Vec<f64>,
    nf: usize,
}

fn final_fields(h: &SoftHistory<'_>, x: &[f64], t: usize) -> FinalFields {
    let m = h.model;
    let l = h.layout;
    let (rho, theta) = h.design();
    let u = &x[..l.n3];
    let p = &x[l.p()..l.p() + l.np];
    let nf = m.fibres.iter().map(Vec::len).max().unwrap_or(0);
    let dt = h.loading.times[t + 1] - h.loading.times[t];
    let mut out = FinalFields {
        stress: Vec::with_capacity(9 * m.ne()),
        jac: Vec::with_capacity(m.ne()),
        stretch: Vec::new(),
        nf,
    };
    for e in 0..m.ne() {
        let d = m.gather(e, u, p);

        let mut hq = [0.0; 6];
        if l.ne_visc > 0 {
            for i in 0..l.nb {
                let off = l.q() + 6 * (e * l.nb + i);
                for k in 0..6 {
                    hq[k] += x[off + k];
                }
            }
        }
        let _ = dt;
        let (s, j, st) = m.element_stress(e, &d, rho[e], theta[e], &hq, 1.0);
        out.stress.extend(s.iter().flatten());
        out.jac.push(j);
        let mut row = st;
        row.resize(nf, 0.0);
        out.stretch.extend(row);
    }
    out
}

fn provenance(p: &Value) -> Value {
    p.get("provenance").cloned().unwrap_or(Value::Null)
}

fn history_of(c: &Case, params: Vec<f64>) -> Result<SoftHistory<'_>, CaeError> {
    SoftHistory::new(&c.model, c.scheme, c.loading.clone(), c.rayleigh, c.newton, params)?
        .with_factorization_reuse(c.factorization_reuse)?
        .with_prescribed_patterns(c.prescribed_patterns.clone())
}

fn unit_params(c: &Case) -> Vec<f64> {
    let ne = c.model.ne();
    let mut p = vec![1.0; ne];
    p.extend(vec![0.0; ne]);
    p
}

#[allow(clippy::too_many_lines)]
fn evaluate(kind: Kind, p: &Value) -> Result<Evaluation, CaeError> {
    let history = kind == Kind::History;
    let c = case(p, history, Interpolation::none())?;
    let h = history_of(&c, unit_params(&c))?;
    let steps = h.run()?;
    let l = h.layout;
    let n = c.model.n();
    let ne = c.model.ne();
    let last = steps.last().ok_or_else(|| CaeError::contract("empty history"))?;
    let x = &last.state;
    let fin = final_fields(&h, x, steps.len() - 1);
    let mut fields: BTreeMap<String, FieldValue> = BTreeMap::new();
    let mut responses: BTreeMap<String, f64> = BTreeMap::new();
    let iterations: Vec<usize> = steps.iter().map(|s| s.iterations).collect();
    let strain = |t: usize, x: &[f64]| Measure::StrainEnergy.value(&h, t, x);
    let t_last = steps.len() - 1;
    if kind == Kind::Static {
        let compliance = Measure::Compliance.value(&h, t_last, x)?;
        responses.insert("stored_energy_J".into(), strain(t_last, x)?);
        responses.insert("compliance_J".into(), compliance);
        responses.insert("peak_displacement_m".into(), peak(&x[..l.n3]));
        responses.insert("minimum_J".into(), fmin(fin.jac.iter().copied()));
        responses.insert("maximum_J".into(), fmax(fin.jac.iter().copied()));
        responses.insert("free_residual_N".into(), last.free_residual);
        fields.insert("displacement_m".into(), field(x[..l.n3].to_vec(), &[n, 3])?);
        let reactions: Vec<f64> =
            (0..l.n3).map(|i| if c.model.fixed[i] { x[l.r() + i] } else { 0.0 }).collect();
        fields.insert("support_reactions_N".into(), field(reactions, &[n, 3])?);
    } else {
        let every = c.output_every;
        let mut times = vec![0.0];
        let x0 = h.initial_state()?;
        let mut disp = vec![x0[..l.n3].to_vec()];
        let mut vel = vec![x0[l.v()..l.v() + l.n3].to_vec()];
        let mut pres = vec![x0[l.p()..l.p() + l.np].to_vec()];
        let mut se = vec![0.0];
        let mv0 = h
            .mass_matrix()
            .matvec(&x0[l.v()..l.v() + l.n3])
            .map_err(|e| CaeError::contract(e.to_string()))?;
        let ke0: f64 = 0.5 * x0[l.v()..l.v() + l.n3].iter().zip(&mv0).map(|(a, b)| a * b).sum::<f64>();
        let mut ke = vec![ke0];
        let mut work = vec![0.0];
        let mut pk = vec![0.0];
        let (mut w_ext, mut d_damp, mut w_visc, mut d_alg) = (0.0, 0.0, 0.0, 0.0);
        let mut prev = x0.clone();
        let (mut mean_se, mut mean_ke, mut wsum) = (0.0, 0.0, 0.0);
        for (t, s) in steps.iter().enumerate() {
            let xs = &s.state;
            let dt = h.loading.times[t + 1] - h.loading.times[t];
            let v = &xs[l.v()..l.v() + l.n3];
            let mv = h.mass_matrix().matvec(v).map_err(|e| CaeError::contract(e.to_string()))?;

            let ledger = h.energy_ledger(t, &prev, xs, None)?;
            w_ext += ledger.external_work + ledger.support_work;
            d_damp += ledger.damping_dissipation;
            w_visc += ledger.viscous_work;
            d_alg += ledger.algorithmic_dissipation;
            let e_s = strain(t, xs)?;
            let e_k = 0.5 * v.iter().zip(&mv).map(|(a, b)| a * b).sum::<f64>();
            mean_se += dt * e_s;
            mean_ke += dt * e_k;
            wsum += dt;
            if (t + 1) % every == 0 || t + 1 == steps.len() {
                times.push(h.loading.times[t + 1]);
                disp.push(xs[..l.n3].to_vec());
                vel.push(v.to_vec());
                pres.push(xs[l.p()..l.p() + l.np].to_vec());
                se.push(e_s);
                ke.push(e_k);
                work.push(w_ext);
                pk.push(peak(&xs[..l.n3]));
            }
            prev.clone_from(xs);
        }
        let e_s = *se.last().unwrap_or(&0.0);
        let e_k = *ke.last().unwrap_or(&0.0);
        responses.insert("final_strain_energy_J".into(), e_s);
        responses.insert("final_kinetic_energy_J".into(), e_k);
        responses.insert("time_mean_strain_energy_J".into(), mean_se / wsum);
        responses.insert("time_mean_kinetic_energy_J".into(), mean_ke / wsum);
        responses.insert("external_work_J".into(), w_ext);
        responses.insert("damping_dissipation_J".into(), d_damp);
        responses.insert("energy_balance_residual_J".into(), w_ext - (e_s + e_k - ke0) - d_damp);
        responses.insert("viscous_work_J".into(), w_visc);
        responses.insert("algorithmic_dissipation_J".into(), d_alg);
        responses.insert("peak_displacement_m".into(), fmax(pk.iter().copied()));
        responses.insert("minimum_J".into(), fmin(fin.jac.iter().copied()));
        let k = times.len();
        fields.insert("time_s".into(), field(times, &[k])?);
        fields.insert("displacement_history_m".into(), field(disp.concat(), &[k, n, 3])?);
        fields.insert("velocity_history_m_s".into(), field(vel.concat(), &[k, n, 3])?);
        if l.np > 0 {
            fields.insert(
                "pressure_history_Pa".into(),
                field(pres.concat().iter().map(|v| -v).collect(), &[k, n])?,
            );
        }
        fields.insert("strain_energy_history_J".into(), field(se, &[k])?);
        fields.insert("kinetic_energy_history_J".into(), field(ke, &[k])?);
        fields.insert("external_work_history_J".into(), field(work, &[k])?);
        fields.insert("peak_displacement_history_m".into(), field(pk, &[k])?);
    }
    fields.insert("cauchy_stress_Pa".into(), field(fin.stress, &[ne, 3, 3])?);
    fields.insert("jacobian".into(), field(fin.jac, &[ne])?);
    if l.np > 0 && kind == Kind::Static {
        fields.insert(
            "hydrostatic_pressure_Pa".into(),
            field(x[l.p()..l.p() + l.np].iter().map(|v| -v).collect(), &[n])?,
        );
    }
    if fin.nf > 0 {
        fields.insert("fibre_stretch".into(), field(fin.stretch, &[ne, fin.nf])?);
    }
    let diagnostics = json!({"notes": kind.notes(), "newton_iterations": iterations, "steps": steps.len(),
        "formulation": if c.model.formulation.mixed() { "mixed_up" } else { "displacement" },
        "laws": c.model.materials.iter().map(|m| m.law.name()).collect::<Vec<_>>(),
        "initial_shear_moduli_Pa": c.model.materials.iter().map(|m| m.mu0).collect::<Vec<_>>(),
        "physical_qualification": false, "provenance": provenance(p)});
    Ok(Evaluation { provider: kind.name().into(), responses, diagnostics: obj(diagnostics), fields })
}

struct Mapped {
    settings: DesignSettings,
    x_density: Vec<f64>,
    x_angle: Vec<f64>,
    params: Vec<f64>,
}

fn mapped(problem: &Value, design_arrays: &NamedArrays) -> Result<Mapped, CaeError> {
    let settings = design(problem)?;
    let ne = settings.case.model.ne();
    let names = design_arrays.names();
    let expected: Vec<&str> = if settings.angle.is_some() { vec![DENSITY, ANGLE] } else { vec![DENSITY] };
    if names.len() != expected.len() || expected.iter().any(|k| !design_arrays.contains(k)) {
        return contract(format!(
            "soft topology design requires exactly the coordinates {}",
            expected.join(", ")
        ));
    }
    let take = |k: &str| -> Result<Vec<f64>, CaeError> {
        let a = design_arrays.get(k).ok_or_else(|| CaeError::contract(format!("missing coordinate {k}")))?;
        if a.len() != ne || a.iter().any(|v| !v.is_finite()) {
            return contract(format!("{k} requires one finite value per element"));
        }
        Ok(a.iter().copied().collect())
    };
    let x_density = take(DENSITY)?;
    if x_density.iter().any(|v| !(0.0..=1.0).contains(v)) {
        return contract("model:soft_density values must lie in [0, 1]");
    }
    let x_angle = if settings.angle.is_some() { take(ANGLE)? } else { vec![0.0; ne] };
    let mut params = settings.density.forward(&x_density);
    params.extend(match &settings.angle {
        Some(a) => a.forward(&x_angle),
        None => vec![0.0; ne],
    });
    Ok(Mapped { settings, x_density, x_angle, params })
}

fn measure(key: &str, settings: &DesignSettings) -> Result<Measure, CaeError> {
    Ok(match key {
        "compliance" => Measure::Compliance,
        "strain_energy" => Measure::StrainEnergy,
        "kinetic_energy" => {
            if !settings.history {
                return contract("kinetic-energy responses require the history analysis");
            }
            Measure::KineticEnergy
        }
        "displacement_squared" => Measure::DisplacementSquared,
        "tracking" => settings
            .case
            .tracking
            .clone()
            .ok_or_else(|| CaeError::contract("tracking responses require case.tracking targets"))?,
        _ => return contract(format!("unknown soft design measure {key}")),
    })
}

fn volume_fraction(m: &Mapped) -> (f64, Vec<f64>) {
    let v = &m.settings.case.model.mesh.volumes;
    let total: f64 = v.iter().sum();
    let ne = v.len();
    let value = v.iter().zip(&m.params[..ne]).map(|(a, b)| a * b).sum::<f64>() / total;
    let g: Vec<f64> = v.iter().map(|a| a / total).collect();
    (value, g)
}

fn evaluate_periodic_design(m: &Mapped, problem: &Value) -> Result<Evaluation, CaeError> {
    let c = &m.settings.case;
    let periodic =
        m.settings.periodic.as_ref().ok_or_else(|| CaeError::contract("internal: no periodic section"))?;
    let ne = c.model.ne();
    let n = c.model.n();
    let volume = volume_fraction(m).0;
    let ev = periodic.evaluate(c, &m.params, volume)?;
    let mut responses = ev.responses;
    responses.insert("volume_fraction".to_string(), volume);
    let mut fields = BTreeMap::new();
    fields.insert("displacement_m".to_string(), field(ev.initial_state[..3 * n].to_vec(), &[n, 3])?);
    fields.insert("density_physical".to_string(), field(m.params[..ne].to_vec(), &[ne])?);
    fields.insert("fibre_angle_rad".to_string(), field(m.params[ne..].to_vec(), &[ne])?);
    for (name, series) in ev.cycles {
        let k = series.len();
        fields.insert(name, field(series, &[k])?);
    }
    Ok(Evaluation {
        provider: Kind::Design.name().into(),
        responses,
        diagnostics: obj(json!({"notes": DESIGN_NOTES, "physical_qualification": false,
            "derivative_scope": "periodic_orbit_exact", "regime": periodic.regime(),
            "periodic_certificate": ev.certificate, "provenance": provenance(&problem["case"])})),
        fields,
    })
}

fn evaluate_design(problem: &Value, design_arrays: &NamedArrays) -> Result<Evaluation, CaeError> {
    let m = mapped(problem, design_arrays)?;
    if m.settings.periodic.is_some() {
        return evaluate_periodic_design(&m, problem);
    }
    let c = &m.settings.case;
    let h = history_of(c, m.params.clone())?;
    let steps = h.run()?;
    let ne = c.model.ne();
    let mut responses = BTreeMap::new();
    let x_last = &steps.last().ok_or_else(|| CaeError::contract("empty history"))?.state;
    for (name, _, (key, mean)) in DESIGN_RESPONSES {
        if key == "volume" {
            responses.insert(name.to_string(), volume_fraction(&m).0);
            continue;
        }
        let Ok(meas) = measure(key, &m.settings) else { continue };
        let value = if mean {
            let total: f64 = m.settings.weights.iter().sum();
            let mut acc = 0.0;
            for (t, s) in steps.iter().enumerate() {
                if m.settings.weights[t] != 0.0 {
                    acc += m.settings.weights[t] / total * meas.value(&h, t, &s.state)?;
                }
            }
            acc
        } else {
            meas.value(&h, steps.len() - 1, x_last)?
        };
        responses.insert(name.to_string(), value);
    }
    let n = c.model.n();
    let mut fields = BTreeMap::new();
    fields.insert("displacement_m".to_string(), field(x_last[..3 * n].to_vec(), &[n, 3])?);
    fields.insert("density_physical".to_string(), field(m.params[..ne].to_vec(), &[ne])?);
    fields.insert("fibre_angle_rad".to_string(), field(m.params[ne..].to_vec(), &[ne])?);
    let _ = (&m.x_density, &m.x_angle);
    Ok(Evaluation {
        provider: Kind::Design.name().into(),
        responses,
        diagnostics: obj(json!({"notes": DESIGN_NOTES, "physical_qualification": false,
            "newton_iterations": steps.iter().map(|s| s.iterations).collect::<Vec<_>>(),
            "derivative_scope": "discrete_history_exact",
            "provenance": provenance(&problem["case"])})),
        fields,
    })
}

fn sensitivities_design(
    problem: &Value,
    design_arrays: &NamedArrays,
    responses: &[String],
) -> Result<DesignSensitivities, CaeError> {
    let mut unique = responses.to_vec();
    unique.sort();
    unique.dedup();
    let m = mapped(problem, design_arrays)?;
    let supported = |r: &String| match &m.settings.periodic {
        None => DESIGN_RESPONSES.iter().any(|(k, _, _)| k == r),
        Some(p) => r == "volume_fraction" || p.responses().contains(r),
    };
    if responses.is_empty() || unique.len() != responses.len() || !responses.iter().all(supported) {
        return contract("unique supported soft design responses required");
    }
    if m.settings.periodic.is_some() {
        return sensitivities_periodic(&m, responses);
    }
    let c = &m.settings.case;
    let ne = c.model.ne();
    let mut h = history_of(c, m.params.clone())?;
    let policy = match m.settings.checkpoint {
        None => Checkpointing::All,
        Some(k) => {
            h.set_cache_capacity(k + 2);
            Checkpointing::Every(k)
        }
    };
    let steps = c.loading.steps();
    let mut values = BTreeMap::new();
    let mut gradients = BTreeMap::new();
    let mut peaks = Vec::new();
    for response in responses {
        let (_, _, (key, mean)) = DESIGN_RESPONSES
            .iter()
            .find(|(k, _, _)| k == response)
            .copied()
            .ok_or_else(|| CaeError::contract("unknown response"))?;
        let (value, grad_params) = if key == "volume" {
            let (v, g) = volume_fraction(&m);
            let mut gp = g;
            gp.extend(vec![0.0; ne]);
            (v, gp)
        } else {
            let objective = Objective {
                history: &h,
                measure: measure(key, &m.settings)?,
                weights: if mean { Some(m.settings.weights.clone()) } else { None },
            };
            let x0 = h.initial_state()?;
            let r = scan_adjoint(&h, &objective, &x0, &m.params, steps, policy)
                .map_err(|e| CaeError::contract(format!("soft history adjoint: {e}")))?;
            peaks.push(r.peak_stored_states);
            let mut gp = r.grad_params;
            let init = h.initial_state_vjp(&x0, &r.grad_initial)?;
            for (a, b) in gp.iter_mut().zip(&init) {
                *a += b;
            }
            (r.value, gp)
        };
        let mut named = NamedArrays::new();
        named.insert(DENSITY, arr(m.settings.density.pullback(&m.x_density, &grad_params[..ne]), &[ne])?);
        if let Some(a) = &m.settings.angle {
            named.insert(ANGLE, arr(a.pullback(&m.x_angle, &grad_params[ne..]), &[ne])?);
        }
        values.insert(response.clone(), value);
        gradients.insert(response.clone(), named);
    }
    Ok(DesignSensitivities {
        responses: values,
        gradients,
        diagnostics: obj(json!({"notes": DESIGN_NOTES, "physical_qualification": false,
            "adjoint": "discrete history adjoint (implexity_ad::scan::scan_adjoint)",
            "derivative_scope": "discrete_history_exact", "peak_stored_states": peaks})),
    })
}

fn sensitivities_periodic(m: &Mapped, responses: &[String]) -> Result<DesignSensitivities, CaeError> {
    let c = &m.settings.case;
    let periodic =
        m.settings.periodic.as_ref().ok_or_else(|| CaeError::contract("internal: no periodic section"))?;
    let ne = c.model.ne();
    let (volume, volume_grad) = volume_fraction(m);
    let program: Vec<String> = responses.iter().filter(|r| *r != "volume_fraction").cloned().collect();
    let (values, certificate) = periodic.gradients(c, &m.params, &program, (volume, &volume_grad))?;
    let mut out_values = BTreeMap::new();
    let mut gradients = BTreeMap::new();
    for response in responses {
        let (value, grad_params) = if response == "volume_fraction" {
            let mut g = volume_grad.clone();
            g.extend(vec![0.0; ne]);
            (volume, g)
        } else {
            values
                .get(response)
                .cloned()
                .ok_or_else(|| CaeError::contract(format!("missing response {response}")))?
        };
        let mut named = NamedArrays::new();
        named.insert(DENSITY, arr(m.settings.density.pullback(&m.x_density, &grad_params[..ne]), &[ne])?);
        if let Some(a) = &m.settings.angle {
            named.insert(ANGLE, arr(a.pullback(&m.x_angle, &grad_params[ne..]), &[ne])?);
        }
        out_values.insert(response.clone(), value);
        gradients.insert(response.clone(), named);
    }
    Ok(DesignSensitivities {
        responses: out_values,
        gradients,
        diagnostics: obj(json!({"notes": DESIGN_NOTES, "physical_qualification": false,
            "derivative_scope": "periodic_orbit_exact", "regime": periodic.regime(),
            "adjoint": "periodic adjoint of the orbit, including the period dependence of autonomous orbits (implexity_solve::periodic)",
            "periodic_certificate": certificate})),
    })
}

#[derive(Debug, Clone, Copy)]
pub struct SoftProvider(pub Kind);

fn problem_value(problem: &ProviderProblem) -> Result<&Value, CaeError> {
    problem
        .downcast_ref::<Value>()
        .ok_or_else(|| CaeError::contract("soft-matter providers require their own problem mapping"))
}

fn no_design(topology: Option<&ArrayD<f64>>) -> Result<(), CaeError> {
    if topology.is_some_and(|t| !t.is_empty()) {
        return contract("This field provider is evaluation-only");
    }
    Ok(())
}

fn validate(kind: Kind, p: &Value) -> Result<(), CaeError> {
    match kind {
        Kind::Static => case(p, false, Interpolation::none()).map(|_| ()),
        Kind::History => case(p, true, Interpolation::none()).map(|_| ()),
        Kind::Design => design(p).map(|_| ()),
    }
}

impl CaeProvider for SoftProvider {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn implementation(&self) -> &str {
        match self.0 {
            Kind::Static => "implexity_physics_solid::soft::providers::SoftProvider(Static)",
            Kind::History => "implexity_physics_solid::soft::providers::SoftProvider(History)",
            Kind::Design => "implexity_physics_solid::soft::providers::SoftProvider(Design)",
        }
    }

    fn capabilities(&self) -> Result<ProviderCapabilities, CaeError> {
        capabilities(self.0)
    }

    fn orchestration_contract(&self) -> Option<Result<PublishedContract, CaeError>> {
        Some(orchestration_contract(self.0).map(|c| PublishedContract::Contract(Box::new(c))))
    }

    fn normalise_problem(&self, problem: &Value) -> Result<ProviderProblem, CaeError> {
        validate(self.0, problem)?;
        Ok(Arc::new(problem.clone()))
    }

    fn preflight(
        &self,
        problem: &ProviderProblem,
        topology: Option<&ArrayD<f64>>,
    ) -> Result<Map<String, Value>, CaeError> {
        let p = problem_value(problem)?;
        validate(self.0, p)?;
        if self.0 == Kind::Design {
            return Ok(obj(
                json!({"ok": true, "requires_complete_design": true, "physical_qualification": false}),
            ));
        }
        no_design(topology)?;
        Ok(obj(
            json!({"ok": true, "equilibrium_solved": false, "optimization_supported": false, "limitations": self.0.notes()}),
        ))
    }

    fn evaluate(&self, problem: &ProviderProblem, topology: &ArrayD<f64>) -> Result<Evaluation, CaeError> {
        if self.0 == Kind::Design {
            return contract("soft_topology_design evaluates named designs only (evaluate_design)");
        }
        no_design(Some(topology))?;
        evaluate(self.0, problem_value(problem)?)
    }

    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        _topology: &ArrayD<f64>,
        _response: &str,
    ) -> Result<Sensitivity, CaeError> {
        contract(format!(
            "{} provides no legacy single-array sensitivity; use sensitivity_design",
            self.0.name()
        ))
    }

    fn coupling_declaration(&self, _problem: Option<&ProviderProblem>) -> Option<Result<Value, CaeError>> {
        Some(Ok(CouplingDeclaration {
            provider: self.0.name().into(),
            active_physics: strings(&["mechanics"]),
            ports: Vec::new(),
            edges: Vec::new(),
            closed_loops: Vec::new(),
            intentionally_frozen: Vec::new(),
            notes: self.0.notes(),
        }
        .to_value()))
    }

    fn interface(&self, name: &str) -> Option<&(dyn Any + Send + Sync)> {
        implexity_optim::provider_ops::design_interface::<Self>(name)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl DesignOperations for SoftProvider {
    fn provides(&self, op: DesignOp) -> bool {
        if self.0 == Kind::Design {
            matches!(
                op,
                DesignOp::EvaluateDesign
                    | DesignOp::PreflightDesign
                    | DesignOp::SensitivityDesign
                    | DesignOp::SensitivitiesDesign
                    | DesignOp::CandidateDesignAdmission
                    | DesignOp::OptimizerLifecycle
            )
        } else {
            matches!(op, DesignOp::Evaluate | DesignOp::EvaluateWithoutDesign)
        }
    }

    fn evaluate_without_design(&self, problem: &ProviderProblem) -> Result<Evaluation, CaeError> {
        evaluate(self.0, problem_value(problem)?)
    }

    fn evaluate_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        _operating_point: usize,
    ) -> Result<Evaluation, CaeError> {
        evaluate_design(problem_value(problem)?, design)
    }

    fn preflight_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> Result<Map<String, Value>, CaeError> {
        mapped(problem_value(problem)?, design)?;
        Ok(obj(json!({"ok": true, "physical_qualification": false})))
    }

    fn sensitivity_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        response: &str,
        _operating_point: usize,
    ) -> Result<DesignSensitivity, CaeError> {
        let mut out = sensitivities_design(problem_value(problem)?, design, &[response.to_string()])?;
        Ok(DesignSensitivity {
            value: out.responses.get(response).copied().unwrap_or(f64::NAN),
            gradients: out.gradients.remove(response).unwrap_or_default(),
            diagnostics: out.diagnostics,
        })
    }

    fn sensitivities_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        responses: &[String],
        _operating_point: usize,
    ) -> Result<DesignSensitivities, CaeError> {
        sensitivities_design(problem_value(problem)?, design, responses)
    }

    fn candidate_admission(
        &self,
        op: DesignOp,
        problem: &ProviderProblem,
        current: &CandidateDesign,
        trial: &CandidateDesign,
    ) -> Result<AdmissionReply, CaeError> {
        if op != DesignOp::CandidateDesignAdmission {
            return Err(CaeError::contract(format!("provider operation {:?} is unavailable", op.name())));
        }
        let named = |c: &CandidateDesign| match c {
            CandidateDesign::Named(n) => Ok(n.clone()),
            CandidateDesign::Array(_) => {
                contract("candidate named designs require nonempty text coordinate ids")
            }
        };
        let (current, trial) = (named(current)?, named(trial)?);
        let evidence = (design_identity(&current)?, design_identity(&trial)?);
        let reply = match evaluate_design(problem_value(problem)?, &trial) {
            Err(e) => json!({"current_design_state_id": evidence.0, "candidate_design_state_id": evidence.1,
                "allow": false, "reason": e.message(), "diagnostics": {"history_admitted": false}}),
            Ok(_) => json!({"current_design_state_id": evidence.0, "candidate_design_state_id": evidence.1,
                "allow": true, "reason": "Complete finite-deformation history admitted", "diagnostics": {"history_admitted": true}}),
        };
        Ok(AdmissionReply::Record(reply))
    }

    fn workspace_declaration(&self, name: &str, problem: Option<&Value>) -> Option<CaeResult<Value>> {
        (self.0 == Kind::Design && name == "study_templates")
            .then(|| Ok(Value::Array(study_templates(problem))))
    }

    fn optimizer_lifecycle(
        &self,
        problem: Option<&ProviderProblem>,
    ) -> Result<LifecycleDeclaration, CaeError> {
        let angle =
            problem.and_then(|p| p.downcast_ref::<Value>()).is_some_and(|p| p.get("fibre_angle").is_some());
        let mut coordinates = vec![DENSITY.to_string()];
        if angle {
            coordinates.push(ANGLE.to_string());
        }
        Ok(LifecycleDeclaration::Typed(OptimizerLifecycleConfig::new(
            coordinates,
            "sensitivity_design",
            "evaluate_design",
            Some("candidate_design_admission"),
            None,
            false,
            false,
        )?))
    }
}


pub fn install(ctx: &implexity_core::packages::InstallContext<'_>) -> CaeResult<()> {
    for kind in ALL {
        ctx.register_provider(Arc::new(SoftProvider(kind)))?;
    }
    Ok(())
}

pub type Steps = Vec<StepSolution>;
