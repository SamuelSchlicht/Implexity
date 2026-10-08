// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value, json};

use implexity_solve::dynamic_program;

use crate::problem::FsiProblem;
use crate::problem::observables::{FLUID_KINDS, SOLID_KINDS};

fn text(title: &str, description: &str) -> Value {
    json!({"title": title, "type": "string", "description": description})
}

fn number(title: &str, description: &str, unit: &str) -> Value {
    json!({"title": title, "type": "number", "description": description, "unit": unit})
}

fn choice(title: &str, description: &str, values: &[&str], default: &str) -> Value {
    json!({"title": title, "type": "string", "enum": values, "default": default, "description": description})
}

fn object(title: &str, description: &str, properties: &Map<String, Value>) -> Value {
    json!({"title": title, "type": "object", "description": description, "properties": properties})
}

fn props(rows: Vec<(&str, Value)>) -> Map<String, Value> {
    rows.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn fluid() -> Value {
    object(
        "Fluid",
        "Moving-occupancy lattice Boltzmann flow on the fixed lattice (physical units).",
        &props(vec![
            (
                "lattice",
                choice(
                    "Velocity set",
                    "D2Q9 (planar, one periodic z layer), D3Q19 or D3Q27 (cumulant collision).",
                    &["D2Q9", "D3Q19", "D3Q27"],
                    "D3Q19",
                ),
            ),
            (
                "shape",
                json!({"title": "Cells per axis", "type": "array", "description": "[nx, ny, nz]; cell i is centred at origin + (i + 1/2) spacing."}),
            ),
            ("spacing_m", number("Cell width", "Lattice spacing.", "m")),
            ("origin_m", json!({"title": "Lower lattice corner", "type": "array", "unit": "m"})),
            (
                "periodic_axes",
                json!({"title": "Periodic axes", "type": "array", "description": "Three Booleans; non-periodic faces are halfway bounce-back walls unless a port sits on them."}),
            ),
            ("density_kg_m3", number("Density", "Reference fluid density.", "kg/m3")),
            (
                "kinematic_viscosity_m2_s",
                number("Kinematic viscosity", "Sets tau+ = 1/2 + 3 nu dt_f / dx^2.", "m2/s"),
            ),
            (
                "collision",
                json!({"title": "Collision", "type": "object", "description": "{kind: bgk | trt (magic, default 3/16) | mrt (bulk_rate, odd_magic, even_rate) | regularized (second-order projection) | cumulant (bulk_rate; D3Q27 only)}."}),
            ),
            (
                "turbulence",
                json!({"title": "Turbulence closure", "type": "object", "description": "{kind: laminar | smagorinsky (constant, norm_floor) | wale (constant, denominator_floor)}; turbulent regimes are refused by the regime detection."}),
            ),
            (
                "coupling_law",
                json!({"title": "Boundary representation of the moving solid", "type": "object", "description": "{kind: psm_superposition (default) | psm (Noble-Torczynski) | brinkman (drag_max_per_s, drag_shape)}; interpolated_bounce_back and immersed_boundary are refused with their reasons."}),
            ),
            (
                "walls",
                json!({"title": "Fixed walls", "type": "array", "description": "Shapes marking wall cells by their centres: {kind: box, box_m} | {kind: cylinder, center_m, radius_m, axis} | {kind: sphere, center_m, radius_m} | {kind: cells, indices}. The push-forward kernel needs two lattice cells between every solid point and a non-periodic lattice face, so a solid attached to a channel wall needs wall layers inside the lattice (wall boxes with the lattice origin below the wall), not the lattice face itself."}),
            ),
            (
                "solid_mask",
                json!({"title": "Wall mask", "type": "object", "description": "Run-length mask {shape, runs: [[start, length]]} over the x-fastest index i + nx (j + ny k); combined with walls."}),
            ),
            (
                "ports",
                json!({"title": "Ports", "type": "array", "description": "{kind: velocity, id, face, mean_m_s, amplitude_m_s, profile: uniform | parabolic | [per cell], frequency_hz, phase_rad, ramp_s} or {kind: pressure, id, face, pressure_pa, amplitude_pa, frequency_hz, phase_rad, ramp_s}."}),
            ),
            (
                "sponges",
                json!({"title": "Absorbing layers", "type": "array", "description": "{face, thickness_cells, strength in (0, 1), pressure_pa, velocity_m_s, wave (optional): {amplitude_m_s, frequency_hz, phase_rad, ramp_s, direction, speed_m_s, origin_m}}; C2-ramped relaxation to the far field (reflection below 1 % at 16 cells). A wave makes the far field a travelling disturbance u + r(t) a sin(2 pi f (t - (x - origin).direction/speed) + phase) (e.g. a gust convected with the free stream, entering through a velocity port with the same frequency and phase at the port plane), so outlet and lateral layers absorb deviations from it instead of damping the gust."}),
            ),
            (
                "body_acceleration_m_s2",
                json!({"title": "Body acceleration", "type": "array", "unit": "m/s2"}),
            ),
            ("initial_velocity_m_s", json!({"title": "Initial velocity", "type": "array", "unit": "m/s"})),
            ("initial_pressure_pa", number("Initial gauge pressure", "Uniform initial pressure.", "Pa")),
            ("mach_limit", number("Mach admission limit", "Default 0.15.", "1")),
            (
                "lattice_velocity_limit",
                number("Lattice velocity limit", "Prescribed velocities in lattice units; default 0.1.", "1"),
            ),
            ("tau_min", number("Relaxation margin", "Admission tau+ > 1/2 + tau_min (default 1e-3).", "1")),
            (
                "sampling",
                choice(
                    "Sampling",
                    "Fluid samples at the last substep or averaged over the macro step.",
                    &["end", "macro_mean"],
                    "end",
                ),
            ),
            (
                "inner_checkpoint_bytes",
                number("Substep checkpoint budget", "Bytes of inner substep snapshots per adjoint.", "B"),
            ),
            (
                "regime",
                json!({"title": "Declared regime", "type": "object", "description": "{velocity_m_s, length_m, reynolds_max}: reference scales of the dimensionless screening and the upper Reynolds number of the exact-gradient regime."}),
            ),
        ]),
    )
}

fn solid() -> Value {
    object(
        "Solid",
        "Total-Lagrangian soft solid on the Kuhn-tetrahedralised reference voxel grid (the design domain).",
        &props(vec![
            (
                "reference_grid",
                json!({"title": "Reference voxel grid", "type": "object", "description": "{origin_m, shape, element_size_m}; plane strain needs one layer of the lattice spacing."}),
            ),
            ("plane_strain", json!({"title": "Plane strain", "type": "boolean"})),
            (
                "material",
                json!({"title": "Material", "type": "object", "description": "A soft-matter law object: law neo_hookean | mooney_rivlin | yeoh | ogden | gent | arruda_boyce | neo_hookean_coupled | st_venant_kirchhoff with its parameters, volumetric {function, bulk_Pa}, optional fibres (HGO), prony {beta, tau_s}, density_kg_m3."}),
            ),
            (
                "material_map",
                json!({"title": "Material map", "type": "array", "description": "Fixed multi-material solid: [{label, region (\"all\", Booleans per voxel, run-length mask or {box_m | boxes_m}), material (a law object) | scale {stiffness (every modulus of the base law), density, prony (\"base\", {beta, tau_s} or {} for none)}}]; later entries override earlier ones; e.g. localized inclusions (stiffer, heavier or more damped regions) or layered bodies."}),
            ),
            (
                "removal_modifier",
                json!({"title": "Removal-zone modifier", "type": "object", "description": "Needs design.removal. {band_m (radius R of the zone around removed material), saturation (removed fraction of the band's material at which the indicator reaches 63 %, default 0.1), stiffness_factor (isochoric stiffness of a saturated zone relative to the base, default 3; below 1 softens) | material (an added isochoric law object), fibre_directions (for an added fibre law), label}: added elastic energy sign sigma(rho) Psi(u; rho) with exact design derivatives; the damping is not modified."}),
            ),
            (
                "fibre_directions",
                json!({"title": "Fibre directions", "type": "array", "description": "1..4 XYZ directions for fibre-reinforced laws (base or material map)."}),
            ),
            (
                "formulation",
                choice(
                    "Formulation",
                    "mixed_up avoids volumetric locking of nearly incompressible solids.",
                    &["mixed_up", "displacement"],
                    "mixed_up",
                ),
            ),
            ("stabilization", number("Pressure stabilisation", "mixed_up only.", "1")),
            ("mass", choice("Mass matrix", "", &["consistent", "lumped"], "consistent")),
            (
                "rayleigh",
                json!({"title": "Rayleigh damping", "type": "object", "description": "{alpha_mass_s_inv, beta_stiffness_s}."}),
            ),
            (
                "supports",
                json!({"title": "Supports", "type": "array", "description": "{box_m | region (voxel mask), components (Booleans or indices), motion: null | {kind: harmonic, amplitude_m, frequency_hz, phase_rad} | {kind: harmonic_rotation, axis, centre_m, amplitude_rad, frequency_hz, phase_rad}}."}),
            ),
            (
                "contact_planes",
                json!({"title": "Rigid contact planes", "type": "array", "description": "{normal, offset_m, activation_m, stiffness_pa, region?}: C2 barrier on the push-forward points (of the region's voxels only when a voxel region is given: one plane per body of a multi-body reference box)."}),
            ),
            (
                "foundations",
                json!({"title": "Elastic foundations", "type": "array", "description": "{box_m, stiffness_n_m} per node."}),
            ),
            (
                "integrator",
                json!({"title": "Time integrator", "type": "object", "description": "{kind: avf_midpoint (gauss_points, default) | generalized_alpha (rho_inf) | newmark (beta, gamma) | quasistatic}."}),
            ),
            (
                "newton",
                json!({"title": "Newton options", "type": "object", "description": "{max_iterations, relative_tolerance, factorization_reuse: false | true | {contraction, max_reuse} (modified Newton inside a step: a factorisation is reused while each step contracts the residual by contraction; the converged root and its derivatives are unchanged)}."}),
            ),
            (
                "inertia_compensation",
                number(
                    "Fluid-inertia compensation",
                    "Fraction of the fluid density removed from the solid density (the saturated cells carry the enclosed fluid with the body).",
                    "1",
                ),
            ),
            ("provenance", text("Provenance", "Material calibration source.")),
        ]),
    )
}

fn design() -> Value {
    object(
        "Design",
        "Density design model:control on the reference voxels (one value per voxel), filtered, projected, interpolated (Wang energy interpolation, Pedersen mass) and pushed forward to the lattice through the fluid-blocking projection.",
        &props(vec![
            (
                "coordinate",
                choice(
                    "Coordinate",
                    "",
                    &[crate::problem::design::COORDINATE],
                    crate::problem::design::COORDINATE,
                ),
            ),
            (
                "region",
                json!({"title": "Design region", "description": "\"all\", Booleans per voxel, run-length mask or {box_m | boxes_m}."}),
            ),
            (
                "removal",
                json!({"title": "Removal-only design", "type": "object", "description": "Material can only be removed from the reference occupancy (initial_density): {reference: initial_density, max_depth_m (largest removable depth below the free surface of the reference), forbidden (voxel selection where nothing is removed), open_faces (grid faces that count as free surface), depth_sharpness_m (smooth maximum of the depth-weighted removal)}; design terms removed_volume_fraction, removed_volume_m3, removal_depth_m; null for a free design."}),
            ),
            (
                "protected_density",
                json!({"title": "Protected density", "description": "Density outside the region (number, list or run-length values)."}),
            ),
            (
                "protected_solid",
                json!({"title": "Protected solid", "description": "Run-length mask of protected voxels at density 1."}),
            ),
            (
                "initial_density",
                json!({"title": "Initial design", "description": "Starting value of model:control (number, list or run-length values)."}),
            ),
            ("filter_radius_m", number("Filter radius", "Volume-weighted hat filter; 0 disables it.", "m")),
            (
                "projection",
                json!({"title": "Projection", "type": "object", "description": "{beta, eta} smoothed Heaviside, or null."}),
            ),
            (
                "interpolation",
                json!({"title": "Interpolation", "type": "object", "description": "{stiffness: simp (penalty) | ramp (q), e_min, wang, wang_beta, wang_eta, mass: pedersen | linear | constant (two-phase stiffness designs of a body of uniform density)}."}),
            ),
            (
                "fluid_blocking",
                json!({"title": "Fluid blocking", "type": "object", "description": "{beta, eta}: projection rho_hat of the pushed-forward occupancy (beta 0 = identity)."}),
            ),
            (
                "symmetry",
                json!({"title": "Mirror symmetry", "type": "object", "description": "{mirror_axis} about the grid mid-plane, or null."}),
            ),
            (
                "two_material",
                json!({"title": "Two-material solid", "description": "Refused (no phase-interpolated stiffness); keep null."}),
            ),
        ]),
    )
}

fn coupling() -> Value {
    object(
        "Coupling",
        "Two-way coupling strength, subcycling and push-forward kernel.",
        &props(vec![
            (
                "mode",
                choice(
                    "Coupling strength",
                    "loose (staggered, added-mass preflight), strong_quasi_newton (IQN-ILS), strong_newton_krylov (monolithic in Schur form); explicit_synchronous is refused.",
                    &["loose", "strong_quasi_newton", "strong_newton_krylov"],
                    "strong_newton_krylov",
                ),
            ),
            ("substeps", number("Fluid substeps per macro step", "m = dt_s / dt_f.", "1")),
            ("predictor_order", number("Loose predictor order", "0..3.", "1")),
            ("tolerance", number("Coupling tolerance", "Strong modes.", "1")),
            ("max_iterations", number("Coupling iterations", "Strong modes.", "1")),
            ("reuse_steps", number("IQN reuse steps", "strong_quasi_newton.", "1")),
            ("initial_relaxation", number("IQN initial relaxation", "strong_quasi_newton.", "1")),
            (
                "krylov",
                json!({"title": "Newton-Krylov inner solver", "type": "object", "description": "{rtol, restart, maxiter}."}),
            ),
            (
                "pushforward",
                json!({"title": "Push-forward kernel", "type": "object", "description": "{kernel: cubic_bspline, width_cells, points_per_axis | points_per_cell_axis (adaptive: at least p points per lattice cell width and axis), saturation_width, blocking_scale, void_threshold (inverted elements with a blocking weight up to it are skipped and counted in the ledger)}."}),
            ),
            (
                "schur_ratio_limit",
                number("Added-mass limit", "Loose coupling refused above this interface Schur ratio.", "1"),
            ),
            (
                "work_defect_action",
                json!({"title": "Work-defect action", "type": "string", "enum": ["refuse", "record"], "default": "refuse",
                       "description": "refuse: loose runs above the limit fail; record: forward evaluations report their values with the failed rule in the certificate (not admitted)."}),
            ),
            (
                "work_defect_limit",
                number(
                    "Work-defect limit",
                    "Loose runs refused above this relative interface work defect.",
                    "1",
                ),
            ),
            ("preflight_modes", number("Preflight modes", "Solid modes of the Schur-ratio preflight.", "1")),
            (
                "field_newton",
                json!({"title": "Field Newton", "type": "object", "description": "{relative_tolerance, max_iterations} of the solid solves inside a coupled step."}),
            ),
            (
                "linear_solves",
                json!({"title": "Certified coupled solves", "type": "object", "description": "{relative_tolerance, restart, max_iterations} of tangent/adjoint solves."}),
            ),
            (
                "step_cache_bytes",
                number("Converged-step cache", "Bytes; replays converged steps bitwise.", "B"),
            ),
            (
                "energy_scale_j",
                number("Energy scale", "Of the work-defect rule (default: cumulative interface work).", "J"),
            ),
        ]),
    )
}

fn time() -> Value {
    object(
        "Time / periodic",
        "Dynamic mode and periodic-state method.",
        &props(vec![
            (
                "kind",
                choice(
                    "Mode",
                    "fixed_horizon (transient or run-to-limit-cycle), periodic_forced, periodic_autonomous (period unknown), steady_stability (onset).",
                    &crate::problem::time::TIME_KINDS,
                    "periodic_forced",
                ),
            ),
            ("period_s", number("Period / horizon period", "fixed_horizon and periodic_forced.", "s")),
            ("steps_per_period", number("Macro steps per period", "N; the macro step is period / N.", "1")),
            ("periods", number("Periods", "fixed_horizon.", "1")),
            (
                "autonomous",
                json!({"title": "Self-excited", "type": "boolean", "description": "fixed_horizon over a self-excited oscillation: smooth windows required."}),
            ),
            ("period_guess_s", number("Period guess", "periodic_autonomous.", "s")),
            ("period_bounds_s", json!({"title": "Period bounds", "type": "array", "unit": "s"})),
            (
                "phase_condition",
                json!({"title": "Phase condition", "type": "object", "description": "{kind: section, sample, level} | {kind: integral}."}),
            ),
            (
                "method",
                json!({"title": "Periodic method", "type": "object", "description": "{kind: picard | newton_krylov (krylov_dimension) | newton_picard (subspace)}."}),
            ),
            ("tolerance", number("Periodic tolerance", "", "1")),
            ("adjoint_tolerance", number("Adjoint tolerance", "", "1")),
            ("spin_up_periods", number("Spin-up periods", "", "1")),
            ("max_periods", number("Period budget", "", "1")),
            ("stability_margin", number("Floquet margin", "", "1")),
            ("floquet_modes", number("Floquet modes", "", "1")),
            (
                "max_adjoint_growth",
                number("Adjoint growth limit", "fixed_horizon (nonperiodic_regime above it).", "1"),
            ),
            ("window_check_tolerance", number("Window check", "fixed_horizon, even periods.", "1")),
            ("step_s", number("Macro step", "steady_stability.", "s")),
            ("modes", number("Modes", "steady_stability.", "1")),
            (
                "spin_up_steps",
                number(
                    "Spin-up steps",
                    "steady_stability: macro steps of the forward map from rest before the steady Newton solve (keeps its first update within the Mach admission).",
                    "1",
                ),
            ),
            (
                "checkpoint",
                json!({"title": "Checkpointing", "type": "object", "description": "{policy: all | binomial | online, ram_snapshots, disk_snapshots}."}),
            ),
            (
                "allow_biased_gradient",
                json!({"title": "Allow biased fallback", "type": "boolean", "description": "Ensemble-window gradients (biased_estimate, never authoritative) when the regime is non-periodic."}),
            ),
        ]),
    )
}

fn observables() -> Value {
    let mut kinds: Vec<&str> = FLUID_KINDS.to_vec();
    kinds.extend(SOLID_KINDS);
    json!({"title": "Observables", "type": "array",
        "description": format!("Named samples after every macro step: {} (solid samples first in the stepper order).", kinds.join(", ")),
        "items": {"type": "object", "properties": {"name": {"type": "string"}, "kind": {"type": "string", "enum": kinds}}}})
}

#[must_use]
pub fn schema(problem: Option<&FsiProblem>) -> Value {
    let responses = match problem {
        Some(p) => dynamic_program::editor_schema_for(
            &p.observables.names(),
            &crate::problem::design_terms(&p.design),
        ),
        None => dynamic_program::editor_schema(),
    };
    let properties = props(vec![
        ("schema", choice("Schema", "", &[crate::problem::SCHEMA], crate::problem::SCHEMA)),
        ("label", text("Label", "")),
        ("provenance", text("Provenance", "")),
        ("fluid", fluid()),
        ("solid", solid()),
        ("design", design()),
        ("coupling", coupling()),
        ("time", time()),
        ("observables", observables()),
        ("responses", responses),
        (
            "operating_points",
            json!({"title": "Operating points", "type": "array", "description": "{label, weight, overrides: {dotted.path[k]: value}}; the job aggregates nominal, expected or smooth worst case."}),
        ),
        (
            "frames",
            json!({"title": "Frames", "type": "object", "description": "{count <= 16, fields: speed | pressure | occupancy | solid_displacement | von_mises | solid_strain | solid_density | removed_density | modifier_intensity, byte_limit}."}),
        ),
    ]);
    json!({"type": "object", "additionalProperties": false, "required": ["schema", "fluid", "solid", "time", "observables", "responses"],
        "properties": properties,
        "x-sections": ["Fluid", "Solid", "Design", "Coupling", "Time / periodic", "Observables", "Responses", "Operating points", "Frames"],
        "x-implexity-options": crate::options::catalogue()})
}
