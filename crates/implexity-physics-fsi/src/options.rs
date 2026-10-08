// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

use implexity_solve::dynamic_program::{PERIOD_KINDS, SERIES_KINDS, WINDOW_KINDS};

pub const SCHEMA: &str = "implexity-fsi-options/1";

fn solid() -> Value {
    json!({
        "laws": [
            {"law": "neo_hookean", "parameters": ["mu_Pa"], "validity": "rubber-like and gel-like solids up to moderate stretches", "derivatives": "exact"},
            {"law": "mooney_rivlin", "parameters": ["c10_Pa", "c01_Pa"], "validity": "silicone elastomers (Ecoflex, Dragon Skin) at moderate stretches", "derivatives": "exact"},
            {"law": "yeoh", "parameters": ["c_Pa"], "validity": "filled elastomers, large stretches", "derivatives": "exact"},
            {"law": "ogden", "parameters": ["mu_Pa", "alpha"], "validity": "soft tissue and elastomers over wide stretch ranges", "derivatives": "exact (spectral derivatives exact at repeated eigenvalues)"},
            {"law": "gent", "parameters": ["mu_Pa", "jm"], "validity": "limiting chain extensibility", "derivatives": "exact", "limitations": "Newton refuses states beyond the extensibility limit"},
            {"law": "arruda_boyce", "parameters": ["mu_Pa", "lambda_m"], "validity": "eight-chain network model", "derivatives": "exact"},
            {"law": "neo_hookean_coupled", "parameters": ["mu_Pa", "lambda_Pa"], "validity": "compressible solids with the coupled log neo-Hookean energy", "derivatives": "exact"},
            {"law": "st_venant_kirchhoff", "parameters": ["mu_Pa", "lambda_Pa"], "validity": "large rotations with small strains (Turek-Hron benchmarks)", "derivatives": "exact", "limitations": "unstable in strong compression"}
        ],
        "volumetric": ["quadratic", "logarithmic", "simo_taylor", "miehe", "ogden_beta", "incompressible (mixed_up only)", "coupled"],
        "fibres": {"model": "Holzapfel-Gasser-Ogden with dispersion kappa", "keys": ["k1_Pa", "k2", "kappa"], "requires": "solid.fibre_directions", "derivatives": "exact"},
        "viscoelasticity": {"model": "Prony series on the isochoric stress (Simo 1987)", "keys": ["prony.beta", "prony.tau_s"], "derivatives": "exact (history variables in the state)", "ledger": "viscous_work_J"},
        "formulation": [
            {"kind": "mixed_up", "validity": "default; nearly incompressible silicones and undrained hydrogels (kappa/mu 1e2..1e4) without volumetric locking", "derivatives": "exact"},
            {"kind": "displacement", "validity": "compressible solids (nu below about 0.45)", "limitations": "locks for nearly incompressible materials"}
        ],
        "mass": ["consistent", "lumped"],
        "damping": {"kind": "rayleigh", "keys": ["alpha_mass_s_inv", "beta_stiffness_s"], "limitations": "stiffness-proportional damping uses the reference stiffness and is not frame-indifferent at large rotations; its dissipation is reported separately"},
        "integrator": [
            {"kind": "avf_midpoint", "parameters": {"gauss_points": 3}, "validity": "default; energy-consistent average-vector-field midpoint (exact for energies polynomial of degree <= 2 n_g along the path, e.g. St. Venant-Kirchhoff with n_g = 2); pairs exactly with the subcycled interface work", "derivatives": "exact", "ledger": "quadrature_defect_J"},
            {"kind": "generalized_alpha", "parameters": {"rho_inf": 0.8}, "validity": "controllable high-frequency dissipation", "derivatives": "exact", "ledger": "algorithmic_dissipation_J"},
            {"kind": "newmark", "parameters": {"beta": 0.25, "gamma": 0.5}, "validity": "trapezoidal rule by default", "derivatives": "exact"},
            {"kind": "quasistatic", "validity": "massless solid", "limitations": "strong coupling only (loose coupling refused: unbounded added-mass ratio)"},
            {"kind": "explicit_central_difference", "status": "not provided", "reason": "no explicit solid substep exists in the soft-matter module (coupling.mode explicit_synchronous is refused)"}
        ],
        "contact": {"kind": "rigid_plane", "keys": ["normal", "offset_m", "activation_m", "stiffness_pa", "region"], "model": "C2 IPC-type barrier on the push-forward points weighted by the fluid-blocking density; with region (voxel mask) only the points of those voxels meet the plane (two bodies in one reference box, e.g. a mid-plane per fold)", "derivatives": "exact (third derivatives included)", "limitations": "rigid planes only: no self-contact, no contact between two deforming bodies (a symmetric pair of bodies closes against a mid-plane per body)"},
        "foundations": {"kind": "elastic_foundation", "keys": ["box_m", "stiffness_n_m"], "derivatives": "exact"},
        "motion": [
            {"kind": "harmonic", "keys": ["amplitude_m", "frequency_hz", "phase_rad"], "validity": "prescribed translation of supported nodes (forced and fixed-horizon kinds)"},
            {"kind": "harmonic_rotation", "keys": ["axis", "centre_m", "amplitude_rad", "frequency_hz", "phase_rad"], "validity": "linearised small-angle rotation (|theta| <= 0.35 rad)", "limitations": "supports with different time signals superpose (one prescribed pattern per signal, e.g. heave + pitch with a phase lag)"}
        ],
        "inertia_compensation": {"validity": "removes a fraction of the fluid density from the solid density: the partially saturated cells carry the fluid inside the body along with it (raw added inertia of rho_f per saturated volume)", "limitations": "refused when less than 5 % of the solid density would remain"}
    })
}

fn coupling() -> Value {
    json!({
        "mode": [
            {"kind": "loose", "parameters": {"predictor_order": 1}, "derivatives": "exact (discrete_history_exact of the staggered recurrence)",
             "validity": "light fluids around heavy solids (solid in gas)", "limitations": "refused when the interface Schur ratio exceeds schur_ratio_limit (added-mass instability) or the cumulative interface work defect exceeds work_defect_limit"},
            {"kind": "strong_quasi_newton", "parameters": {"tolerance": 1e-8, "max_iterations": 50, "reuse_steps": 0, "initial_relaxation": 0.5},
             "derivatives": "exact at the converged coupling residual", "validity": "moderate added mass", "limitations": "secant reuse across steps reproduces bitwise only within the step cache"},
            {"kind": "strong_newton_krylov", "parameters": {"tolerance": 1e-10, "max_iterations": 30, "krylov": {"rtol": 1e-4, "restart": 30, "maxiter": 200}},
             "derivatives": "exact (monolithic coupled step in Schur form)", "validity": "default; any density ratio including rho_s/rho_f <= 1"},
            {"kind": "explicit_synchronous", "status": "refused", "reason": "no explicit solid substep inside the fluid substeps exists"}
        ],
        "subcycling": {"key": "substeps", "validity": "m fluid steps per solid macro step; the fluid sees the straight path of the solid between the macro-step endpoints (admission: displacement per macro step <= half the kernel width)"},
        "pushforward": {"kernel": "cubic_bspline", "keys": ["width_cells", "points_per_axis", "saturation_width", "blocking_scale"], "validity": "diffuse interface of 4 h cells; points closer than the kernel width", "derivatives": "exact"},
        "admission": ["schur_ratio_limit (loose)", "work_defect_limit (loose)", "field_newton", "linear_solves (certified tangent/adjoint solves)"]
    })
}

fn time() -> Value {
    json!({
        "kind": [
            {"kind": "fixed_horizon", "derivative_scope": "discrete_history_exact", "validity": "transients and run-to-limit-cycle averages; autonomous oscillations need smooth windows (hann, bump, tukey)", "regime_detection": "adjoint growth per period above max_adjoint_growth refuses (nonperiodic_regime); optional window check"},
            {"kind": "periodic_forced", "derivative_scope": "periodic_orbit_exact", "validity": "externally periodic forcing (oscillating ports, prescribed support motion) with a stable orbit", "limitations": "every forcing frequency must be a multiple of 1/period; no ramps"},
            {"kind": "periodic_autonomous", "derivative_scope": "periodic_orbit_exact", "validity": "self-excited oscillations at steady driving (flutter-type, flow-induced vibration); the period is an unknown with an exact gradient", "limitations": "Picard refused; steady ports only; unstable orbits refused by the Floquet gate"},
            {"kind": "steady_stability", "derivative_scope": "stability_eigenvalue_exact", "validity": "onset of oscillation: growth rate and frequency of the least stable mode of the steady state", "limitations": "gradients of growth_rate_per_s only (refused with the reason for options without second-order capability); spin_up_steps starts the steady Newton solve from a transient"}
        ],
        "method": [
            {"kind": "picard", "validity": "forced orbits, run to the limit cycle"},
            {"kind": "newton_krylov", "parameters": {"krylov_dimension": 30}, "validity": "default shooting"},
            {"kind": "newton_picard", "parameters": {"subspace": 8}, "validity": "few slow modes (large LBM states)", "limitations": "fixed subspace dimension (no adaptive enlargement)"}
        ],
        "checkpoint": [
            {"policy": "all", "validity": "tiny histories"},
            {"policy": "binomial", "parameters": {"ram_snapshots": 16, "disk_snapshots": 0}, "validity": "optimal revolve schedules, gradients bitwise equal to store-all"},
            {"policy": "online", "validity": "unknown step counts"}
        ],
        "chaotic_regimes": {"policy": "refused (nonperiodic_regime); ensemble-averaged short-window gradients only with allow_biased_gradient, reported as biased_estimate and never authoritative; least-squares shadowing deferred"}
    })
}

fn averaging() -> Value {
    json!({
        "program_schema": implexity_solve::dynamic_program::SCHEMA,
        "windows": WINDOW_KINDS,
        "series_functionals": SERIES_KINDS,
        "period_terms": PERIOD_KINDS,
        "design_terms": [crate::problem::DESIGN_VOLUME],
        "removal_design_terms": crate::problem::removal::REMOVAL_TERMS,
        "composite_terms": implexity_solve::dynamic_program::COMPOSITE_KINDS,
        "operating_points": {"aggregation": ["nominal", "expected (weights)", "smooth_worst_case"], "note": "the job layer aggregates the exact per-point gradients"},
        "statement": "the weighted time mean of total gradients IS the gradient of the weighted mean (one adjoint sweep with weighted sources); frozen-load gradient averages are never offered as gradients"
    })
}

fn design() -> Value {
    json!({
        "coordinate": crate::problem::design::COORDINATE,
        "interpolation": {"stiffness": ["simp", "ramp"], "wang_energy_interpolation": true, "mass": ["pedersen", "linear", "constant"]},
        "filter": "volume-weighted hat filter on the reference voxels",
        "removal_only": {"key": "design.removal", "bound": "rho <= reference occupancy (exact, a linear map)", "restrictions": ["max_depth_m", "forbidden", "open_faces"], "derivatives": "exact"},
        "material_map": {"key": "solid.material_map", "entries": "region -> law object or scaled base (stiffness, density, prony)", "derivatives": "fixed data (the density interpolation acts on every material)"},
        "removal_zone_modifier": {"key": "solid.removal_modifier", "model": "added isochoric energy sign sigma(rho) Psi(u; rho), sigma = 1 - exp(-s/s0), s the kernel-averaged removed fraction within band_m", "derivatives": "exact first derivatives; no third derivatives (steady-state growth-rate gradients refused)", "limitation": "viscous damping is not modified (no design-dependent damping channel in the soft model)"},
        "projection": "smoothed Heaviside (beta, eta)",
        "fluid_blocking": "second projection rho_hat (beta_f, eta_f) of the pushed-forward occupancy",
        "symmetry": "mirror about a mid-plane of the reference grid",
        "two_material": {"status": "refused", "reason": "one material per element in the soft model (no phase-interpolated stiffness)"}
    })
}

pub const RULES: [(&str, &str, &str); 22] = [
    (
        "fluid.collision.kind = cumulant with a lattice other than D3Q27",
        "refused",
        "the cumulant collision is implemented for D3Q27 only",
    ),
    (
        "fluid.lattice = D2Q9 with a three-dimensional solid",
        "refused",
        "planar lattices need a plane-strain solid of the layer thickness",
    ),
    (
        "plane-strain solid with element_size_m != fluid.spacing_m",
        "refused",
        "the pushed-forward volume per layer must equal the solid thickness",
    ),
    (
        "push-forward point spacing above the kernel width",
        "refused",
        "sparse points leave holes in the occupancy",
    ),
    (
        "fluid.coupling_law = interpolated_bounce_back",
        "refused",
        "non-differentiable verification mode for prescribed rigid motion",
    ),
    (
        "fluid.coupling_law = immersed_boundary",
        "refused",
        "not provided (reduces to brinkman for density designs)",
    ),
    ("coupling.mode = explicit_synchronous", "refused", "no explicit solid substep exists"),
    (
        "coupling.mode = loose with solid.integrator = quasistatic",
        "refused",
        "massless solid: unbounded added-mass ratio",
    ),
    (
        "coupling.mode = loose with an interface Schur ratio above schur_ratio_limit",
        "refused at preflight/evaluation",
        "added-mass instability of staggered coupling",
    ),
    (
        "coupling.mode = loose with a cumulative work defect above work_defect_limit",
        "refused after the run",
        "the staggered interface does not transmit energy consistently",
    ),
    ("time.kind = periodic_autonomous with method picard", "refused", "no period update"),
    (
        "fixed horizon declared autonomous with uniform, trapezoid or custom windows",
        "refused",
        "windowing theorem: O(1) phase-drift gradient error",
    ),
    (
        "period or frequency response on a fixed horizon",
        "refused",
        "the period is defined on periodic orbits only (use crossing_period)",
    ),
    (
        "periodic_forced with a forcing frequency not a multiple of 1/period or a ramp",
        "refused",
        "the forcing must be periodic with the declared period",
    ),
    (
        "periodic_autonomous or steady_stability with oscillating ports or prescribed motion",
        "refused",
        "an autonomous problem must be unforced",
    ),
    (
        "steady_stability gradients of sample responses and of the angular frequency",
        "refused",
        "only growth_rate_per_s has an exact gradient (stability_eigenvalue_exact); it is refused for options without second-order capability (interface_power samples, cumulant, WALE, Ogden)",
    ),
    (
        "unstable periodic orbit (Floquet multiplier within stability_margin of the unit circle)",
        "refused (periodic_orbit_unstable)",
        "no physical run reaches it",
    ),
    (
        "adjoint growth above max_adjoint_growth, non-converging periodic residual",
        "refused (nonperiodic_regime)",
        "chaotic or unsteady regime: no authoritative gradient",
    ),
    ("design.two_material", "refused", "no phase-interpolated stiffness in the soft model"),
    (
        "prescribed motions with different time signals",
        "admitted",
        "one prescribed pattern per signal (heave + pitch with a phase lag); in periodic_forced every signal needs f·T integer",
    ),
    (
        "Reynolds number above the declared regime (fluid.regime.reynolds_max)",
        "screened (preflight warning, regime monitor metric)",
        "exact gradients need laminar periodic flow",
    ),
    (
        "tau+ at or below 1/2 + tau_min, Mach or lattice velocity above their limits",
        "refused (moving LBM admission)",
        "lattice Boltzmann stability and compressibility error",
    ),
];

#[must_use]
pub fn catalogue() -> Value {
    json!({
        "schema": SCHEMA,
        "fluid": implexity_physics_lbm::moving::options::catalogue(),
        "solid": solid(),
        "coupling": coupling(),
        "time": time(),
        "objective_averaging": averaging(),
        "design": design(),
        "combination_rules": RULES.iter().map(|(r, e, why)| json!({"rule": r, "effect": e, "reason": why})).collect::<Vec<_>>(),
        "derivative_scopes": {
            "discrete_history_exact": "gradient of the objective as computed by the discrete history",
            "periodic_orbit_exact": "gradient over the discrete periodic orbit (implicit-function theorem on the period map)",
            "stability_eigenvalue_exact": "simple eigenvalue of the linearised step map (growth-rate gradients through the second-order adjoint of the coupled step)",
            "biased_estimate": "ensemble fallback for non-periodic regimes; never authoritative"
        }
    })
}
