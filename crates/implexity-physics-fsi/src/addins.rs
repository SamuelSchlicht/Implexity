// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::any::Any;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use implexity_core::CaeResult;
use implexity_core::extensions::{CallbackError, MetricSpec};
use implexity_core::orchestration::{
    AddInAdapter, AddInCategory, AddInContract, ContractInput, ExecutionKind, Fidelity,
};
use implexity_core::packages::{InstallContext, link_installer};

use crate::provider::option_port;

pub const INSTALLER: &str = "implexity.addins.fluid_structure_dynamics";
pub const PACKAGE: &str = "fluid_structure_dynamics";

pub type OptionRow = (&'static str, &'static str, &'static str, bool, fn() -> Value, &'static str);

pub const OPTIONS: [OptionRow; 38] = [
    (
        "fsi_velocity_set",
        "fluid",
        "fsi_lattice_d2q9",
        false,
        || json!({"fluid": {"lattice": "D2Q9"}}),
        "planar flows on one periodic lattice layer (needs a plane-strain solid); exact derivatives",
    ),
    (
        "fsi_velocity_set",
        "fluid",
        "fsi_lattice_d3q19",
        true,
        || json!({"fluid": {"lattice": "D3Q19"}}),
        "three-dimensional weakly compressible flow (default); exact derivatives",
    ),
    (
        "fsi_velocity_set",
        "fluid",
        "fsi_lattice_d3q27",
        false,
        || json!({"fluid": {"lattice": "D3Q27"}}),
        "three-dimensional flow, required by the cumulant collision; about 1.4x the cost of D3Q19",
    ),
    (
        "fsi_collision_model",
        "fluid",
        "fsi_collision_trt",
        true,
        || json!({"fluid": {"collision": {"kind": "trt", "magic": 0.1875}}}),
        "two-relaxation-time with magic 3/16 (default): wall location independent of viscosity; exact derivatives",
    ),
    (
        "fsi_collision_model",
        "fluid",
        "fsi_collision_bgk",
        false,
        || json!({"fluid": {"collision": {"kind": "bgk"}}}),
        "single relaxation time; wall location depends on viscosity; exact derivatives",
    ),
    (
        "fsi_collision_model",
        "fluid",
        "fsi_collision_mrt",
        false,
        || json!({"fluid": {"collision": {"kind": "mrt"}}}),
        "multiple relaxation times (defaults reproduce TRT); separate bulk and ghost rates; exact derivatives",
    ),
    (
        "fsi_collision_model",
        "fluid",
        "fsi_collision_regularized",
        false,
        || json!({"fluid": {"collision": {"kind": "regularized"}}}),
        "second-order Hermite projection with Guo forcing; exact derivatives; wall and resolution accuracy require validation",
    ),
    (
        "fsi_collision_model",
        "fluid",
        "fsi_collision_cumulant",
        false,
        || json!({"fluid": {"lattice": "D3Q27", "collision": {"kind": "cumulant", "bulk_rate": 1.0}}}),
        "cumulant collision (D3Q27 only) for low viscosity; exact first derivatives, no second-order action",
    ),
    (
        "fsi_turbulence_closure",
        "fluid",
        "fsi_turbulence_laminar",
        true,
        || json!({"fluid": {"turbulence": {"kind": "laminar"}}}),
        "resolved laminar or periodic flow (default, the exact-gradient regime)",
    ),
    (
        "fsi_turbulence_closure",
        "fluid",
        "fsi_turbulence_smagorinsky",
        false,
        || json!({"fluid": {"turbulence": {"kind": "smagorinsky", "constant": 0.17}}}),
        "Smagorinsky LES closure (regularised, exact derivatives); turbulent regimes are refused by the regime detection",
    ),
    (
        "fsi_turbulence_closure",
        "fluid",
        "fsi_turbulence_wale",
        false,
        || json!({"fluid": {"turbulence": {"kind": "wale", "constant": 0.5}}}),
        "WALE LES closure (exact first derivatives, no second-order action); same regime restriction",
    ),
    (
        "fsi_boundary_representation",
        "fluid",
        "fsi_boundary_psm_superposition",
        true,
        || json!({"fluid": {"coupling_law": {"kind": "psm_superposition"}}}),
        "partially saturated cells with the superposition operator (default; stable for moving saturated bodies); exact derivatives",
    ),
    (
        "fsi_boundary_representation",
        "fluid",
        "fsi_boundary_psm",
        false,
        || json!({"fluid": {"coupling_law": {"kind": "psm"}}}),
        "partially saturated cells, Noble-Torczynski operator; saturated bodies in sustained motion can become unstable",
    ),
    (
        "fsi_boundary_representation",
        "fluid",
        "fsi_boundary_brinkman",
        false,
        || json!({"fluid": {"coupling_law": {"kind": "brinkman", "drag_max_per_s": 1000.0, "drag_shape": 8.0}}}),
        "Brinkman penalization (RAMP resistance); no-slip only as drag_max dt_f grows",
    ),
    (
        "fsi_solid_law",
        "solid",
        "fsi_law_neo_hookean",
        false,
        || json!({"solid": {"material": {"law": "neo_hookean", "mu_Pa": 5000.0, "volumetric": {"function": "quadratic", "bulk_Pa": 250_000.0}, "density_kg_m3": 1100.0}}}),
        "neo-Hookean (rubber-like, gel-like solids)",
    ),
    (
        "fsi_solid_law",
        "solid",
        "fsi_law_mooney_rivlin",
        true,
        || json!({"solid": {"material": {"law": "mooney_rivlin", "c10_Pa": 2000.0, "c01_Pa": 500.0, "volumetric": {"function": "quadratic", "bulk_Pa": 250_000.0}, "density_kg_m3": 1100.0}}}),
        "Mooney-Rivlin (silicone elastomers; default)",
    ),
    (
        "fsi_solid_law",
        "solid",
        "fsi_law_ogden",
        false,
        || json!({"solid": {"material": {"law": "ogden", "mu_Pa": [5000.0], "alpha": [2.0], "volumetric": {"function": "quadratic", "bulk_Pa": 250_000.0}, "density_kg_m3": 1100.0}}}),
        "N-term Ogden (soft tissue); exact at repeated principal stretches",
    ),
    (
        "fsi_solid_law",
        "solid",
        "fsi_law_hgo",
        false,
        || json!({"solid": {"material": {"law": "neo_hookean", "mu_Pa": 5000.0, "volumetric": {"function": "quadratic", "bulk_Pa": 250_000.0}, "fibres": {"k1_Pa": 10_000.0, "k2": 5.0, "kappa": 0.1}, "density_kg_m3": 1100.0}, "fibre_directions": [[1.0, 0.0, 0.0]]}}),
        "Holzapfel-Gasser-Ogden fibre reinforcement with dispersion on a neo-Hookean matrix",
    ),
    (
        "fsi_solid_law",
        "solid",
        "fsi_law_st_venant_kirchhoff",
        false,
        || json!({"solid": {"material": {"law": "st_venant_kirchhoff", "mu_Pa": 500_000.0, "lambda_Pa": 2_000_000.0, "density_kg_m3": 1000.0}, "formulation": "displacement"}}),
        "St. Venant-Kirchhoff (large rotations, small strains; Turek-Hron)",
    ),
    (
        "fsi_solid_law",
        "solid",
        "fsi_law_viscoelastic_prony",
        false,
        || json!({"solid": {"material": {"law": "neo_hookean", "mu_Pa": 5000.0, "volumetric": {"function": "quadratic", "bulk_Pa": 250_000.0}, "prony": {"beta": [0.3], "tau_s": [0.01]}, "density_kg_m3": 1100.0}}}),
        "finite-strain viscoelasticity (Prony series, Simo 1987); viscous work in the ledger",
    ),
    (
        "fsi_solid_formulation",
        "solid",
        "fsi_formulation_mixed_up",
        true,
        || json!({"solid": {"formulation": "mixed_up"}}),
        "mixed u-p with pressure projection (default): no locking for nearly incompressible solids",
    ),
    (
        "fsi_solid_formulation",
        "solid",
        "fsi_formulation_displacement",
        false,
        || json!({"solid": {"formulation": "displacement"}}),
        "displacement T4 (compressible solids; locks near incompressibility)",
    ),
    (
        "fsi_time_integrator",
        "solid",
        "fsi_integrator_avf_midpoint",
        true,
        || json!({"solid": {"integrator": {"kind": "avf_midpoint", "gauss_points": 3}}}),
        "energy-consistent AVF midpoint (default); pairs exactly with the subcycled interface work",
    ),
    (
        "fsi_time_integrator",
        "solid",
        "fsi_integrator_generalized_alpha",
        false,
        || json!({"solid": {"integrator": {"kind": "generalized_alpha", "rho_inf": 0.8}}}),
        "generalised-alpha with high-frequency dissipation rho_inf; algorithmic dissipation in the ledger",
    ),
    (
        "fsi_time_integrator",
        "solid",
        "fsi_integrator_newmark",
        false,
        || json!({"solid": {"integrator": {"kind": "newmark", "beta": 0.25, "gamma": 0.5}}}),
        "Newmark (trapezoidal by default)",
    ),
    (
        "fsi_solid_damping",
        "solid",
        "fsi_damping_none",
        true,
        || json!({"solid": {"rayleigh": {"alpha_mass_s_inv": 0.0, "beta_stiffness_s": 0.0}}}),
        "no structural damping (default; viscoelastic laws carry their own dissipation)",
    ),
    (
        "fsi_solid_damping",
        "solid",
        "fsi_damping_rayleigh",
        false,
        || json!({"solid": {"rayleigh": {"alpha_mass_s_inv": 1.0, "beta_stiffness_s": 1e-4}}}),
        "Rayleigh damping (reference stiffness; not frame-indifferent at large rotations)",
    ),
    (
        "fsi_solid_contact",
        "solid",
        "fsi_contact_none",
        true,
        || json!({"solid": {"contact_planes": []}}),
        "no contact (default)",
    ),
    (
        "fsi_solid_contact",
        "solid",
        "fsi_contact_rigid_plane",
        false,
        || json!({"solid": {"contact_planes": [{"normal": [0.0, -1.0, 0.0], "offset_m": -0.01, "activation_m": 2e-4, "stiffness_pa": 1000.0}]}}),
        "rigid-plane contact (C2 barrier; e.g. a symmetry plane); no self-contact",
    ),
    (
        "fsi_coupling_strength",
        "coupling",
        "fsi_coupling_strong_newton_krylov",
        true,
        || json!({"coupling": {"mode": "strong_newton_krylov"}}),
        "monolithic coupled step in Schur form (default; any density ratio)",
    ),
    (
        "fsi_coupling_strength",
        "coupling",
        "fsi_coupling_strong_quasi_newton",
        false,
        || json!({"coupling": {"mode": "strong_quasi_newton"}}),
        "IQN-ILS partitioned strong coupling",
    ),
    (
        "fsi_coupling_strength",
        "coupling",
        "fsi_coupling_loose",
        false,
        || json!({"coupling": {"mode": "loose", "predictor_order": 1}}),
        "staggered loose coupling (solid in gas); refused above the added-mass Schur-ratio limit",
    ),
    (
        "fsi_periodic_state_method",
        "time",
        "fsi_periodic_newton_krylov",
        true,
        || json!({"time": {"method": {"kind": "newton_krylov", "krylov_dimension": 30}}}),
        "Newton-Krylov shooting (default)",
    ),
    (
        "fsi_periodic_state_method",
        "time",
        "fsi_periodic_newton_picard",
        false,
        || json!({"time": {"method": {"kind": "newton_picard", "subspace": 8}}}),
        "Newton-Picard shooting (few slow modes)",
    ),
    (
        "fsi_periodic_state_method",
        "time",
        "fsi_periodic_picard",
        false,
        || json!({"time": {"method": {"kind": "picard"}}}),
        "run to the limit cycle (forced orbits only)",
    ),
    (
        "fsi_objective_averaging",
        "time",
        "fsi_averaging_periodic",
        true,
        || json!({"responses": {"window": {"kind": "periodic"}}}),
        "exact cycle average over one period of the orbit (default)",
    ),
    (
        "fsi_objective_averaging",
        "time",
        "fsi_averaging_hann",
        false,
        || json!({"responses": {"window": {"kind": "hann", "from": 0, "to": 1}}}),
        "smooth Hann window of a fixed horizon (required for self-excited run-to-limit-cycle averages; set from/to)",
    ),
    (
        "fsi_objective_averaging",
        "time",
        "fsi_averaging_custom",
        false,
        || json!({"responses": {"window": {"kind": "custom", "weights": [1.0]}}}),
        "authored phase weights (forced or periodic samples; refused for autonomous fixed horizons)",
    ),
];

#[derive(Debug)]
pub struct FsiOptionAdapter {
    patch: Value,
}

impl AddInAdapter for FsiOptionAdapter {
    fn implementation(&self) -> String {
        "implexity_physics_fsi::addins::FsiOptionAdapter".into()
    }

    fn component_kind(&self) -> Option<String> {
        Some("fsi_physics_option".into())
    }

    fn authoring_contract(&self) -> Option<Map<String, Value>> {
        let mut m = Map::new();
        m.insert("problem_patch".into(), self.patch.clone());
        m.insert("problem_schema".into(), json!(crate::problem::SCHEMA));
        Some(m)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn option_contract(ctx: &InstallContext<'_>, row: &OptionRow) -> CaeResult<AddInContract> {
    let (quantity, domain, id, default, patch, notes) = row;
    let mut c = AddInContract::new(*id);
    c.category = AddInCategory::Constitutive;
    c.provides = vec![option_port(quantity, domain)];
    c.fidelity = Fidelity::Intermediate;
    c.priority = if *default { 20 } else { 10 };
    c.scope = vec!["*".into()];
    c.exact_design_derivatives = Some(true);
    c.exact_state_transpose = Some(true);
    c.notes = vec![
        (*notes).to_string(),
        format!("problem patch: {}", patch()),
        "Physics-option component of lattice_boltzmann_fsi_dynamic; numerical execution is owned by the provider.".into(),
    ];
    c.contract_version = 2;
    c.compatibility_mode = false;
    c.owner_id = ctx.owner_id().to_string();
    c.execution_kind = Some(ExecutionKind::Operation);
    c.supported_operations = vec!["resolve_component".into()];
    c.checked()
}


pub fn install(ctx: &InstallContext<'_>) -> CaeResult<()> {
    crate::provider::install(ctx)?;
    crate::moving_contact::event_provider::install(ctx)?;
    crate::moving_contact::closed_surface_provider::install(ctx)?;
    crate::moving_contact::macro_control::provider::install(ctx)?;
    crate::moving_contact::shell_forward_provider::install(ctx)?;
    for row in &OPTIONS {
        let contract = option_contract(ctx, row)?;
        ctx.register_addin(
            ContractInput::Typed(Box::new(contract)),
            Some(Arc::new(FsiOptionAdapter { patch: (row.4)() })),
        )?;
    }
    let metrics = crate::regime::METRICS
        .iter()
        .map(|(id, unit)| MetricSpec::new(id, unit, false))
        .collect::<CaeResult<Vec<_>>>()?;
    let monitor = |problem: &Value, diagnostics: Option<&Value>| {
        crate::regime::monitor(problem, diagnostics)
            .map_err(|message| CallbackError { kind: "ValueError".into(), message })
    };
    ctx.register_regime_monitor(crate::regime::MONITOR, Arc::new(monitor), Some(metrics), None)
}

pub fn link() {
    link_installer(INSTALLER, install);
}
